use ahash::AHashMap;
use smallvec::SmallVec;
use std::sync::Arc;

use crate::model::{Matcher, MatcherOperator, SeriesId};
use crate::packed_model::PackedLabelSet;
use crate::utils::algorithms::set_ops::{intersect_in_place, subtract_in_place};

const INLINE_SUBSETS: usize = 8;

#[derive(Default)]
struct NameEntry {
    values: AHashMap<String, Vec<SeriesId>>,
    any: Vec<SeriesId>,
}

impl NameEntry {
    fn insert(&mut self, value: &str, id: SeriesId) {
        self.values.entry(value.to_owned()).or_default().push(id);

        self.any.push(id);
    }
}

#[derive(Default)]
struct Postings {
    entries: AHashMap<String, NameEntry>,
    all: Vec<SeriesId>,
}

impl Postings {
    fn add_series(&mut self, id: SeriesId, labels: &PackedLabelSet) {
        for (name, value) in labels {
            self.entries
                .entry(name.to_owned())
                .or_default()
                .insert(value, id);
        }

        self.all.push(id);
    }
}

#[derive(Default)]
pub struct IndexV11APacked {
    forward: AHashMap<SeriesId, Arc<PackedLabelSet>>,
    inverted: AHashMap<Arc<PackedLabelSet>, SeriesId>,
    postings: Postings,
    next_id: u64,
}

impl IndexV11APacked {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn lookup(&self, labels: &PackedLabelSet) -> Option<SeriesId> {
        self.inverted.get(labels).copied()
    }

    pub fn encode(&mut self, labels: &PackedLabelSet) -> SeriesId {
        if let Some(id) = self.lookup(labels) {
            return id;
        }

        let id = SeriesId(self.next_id);
        self.next_id += 1;

        let shared = Arc::new(labels.clone());

        self.forward.insert(id, Arc::clone(&shared));
        self.inverted.insert(Arc::clone(&shared), id);
        self.postings.add_series(id, &shared);

        id
    }

    #[inline]
    pub fn labels_for(&self, id: SeriesId) -> Option<Arc<PackedLabelSet>> {
        self.forward.get(&id).cloned()
    }

    #[inline]
    pub fn with_label(&self, name: &str) -> Option<Vec<SeriesId>> {
        self.postings
            .entries
            .get(name)
            .map(|entry| entry.any.clone())
    }

    pub fn resolve(&self, matchers: &[Matcher]) -> Vec<SeriesId> {
        if matchers.is_empty() {
            return self.postings.all.clone();
        }

        let mut intersectors: SmallVec<[PostingSubset; INLINE_SUBSETS]> = SmallVec::new();
        let mut excluders: SmallVec<[PostingSubset; INLINE_SUBSETS]> = SmallVec::new();

        for matcher in matchers {
            match matcher.operator {
                MatcherOperator::Equal => {
                    if matcher.value.is_empty() {
                        let exc = PostingSubset::new_any(&matcher.name, &self.postings);

                        if exc.estimated_size() != 0 {
                            excluders.push(exc);
                        }
                    } else {
                        let int =
                            PostingSubset::new_exact(&matcher.name, &matcher.value, &self.postings);

                        if int.estimated_size() == 0 {
                            return vec![];
                        }

                        intersectors.push(int);
                    }
                }

                MatcherOperator::NotEqual => {
                    if matcher.value.is_empty() {
                        let int = PostingSubset::new_any(&matcher.name, &self.postings);

                        if int.estimated_size() == 0 {
                            return vec![];
                        }

                        intersectors.push(int);
                    } else {
                        let exc =
                            PostingSubset::new_exact(&matcher.name, &matcher.value, &self.postings);

                        if exc.estimated_size() != 0 {
                            excluders.push(exc);
                        }
                    }
                }
            }
        }

        intersectors.sort_unstable_by_key(PostingSubset::estimated_size);
        excluders.sort_unstable_by_key(|sub| std::cmp::Reverse(sub.estimated_size()));

        let mut result = if intersectors.is_empty() {
            self.postings.all.clone()
        } else {
            let opt = match intersectors[0] {
                PostingSubset::Exact { name, value, .. } => self
                    .postings
                    .entries
                    .get(name)
                    .and_then(|entry| entry.values.get(value))
                    .cloned(),

                PostingSubset::Any { name, .. } => self
                    .postings
                    .entries
                    .get(name)
                    .map(|entry| entry.any.clone()),
            };

            if let Some(ids) = opt {
                ids
            } else {
                return vec![];
            }
        };

        for subset in intersectors.iter().skip(1) {
            Self::intersect_with_subset(&mut result, subset, &self.postings);

            if result.is_empty() {
                return result;
            }
        }

        for subset in &excluders {
            Self::exclude_subset(&mut result, subset, &self.postings);

            if result.is_empty() {
                return result;
            }
        }

        result
    }

    #[inline]
    fn intersect_with_subset(
        keep: &mut Vec<SeriesId>,
        subset: &PostingSubset<'_>,
        postings: &Postings,
    ) {
        match subset {
            PostingSubset::Exact { name, value, .. } => {
                if let Some(ids) = postings
                    .entries
                    .get(*name)
                    .and_then(|entry| entry.values.get(*value))
                {
                    intersect_in_place(keep, ids);
                }
            }

            PostingSubset::Any { name, .. } => {
                if let Some(entry) = postings.entries.get(*name) {
                    intersect_in_place(keep, &entry.any);
                }
            }
        }
    }

    #[inline]
    fn exclude_subset(keep: &mut Vec<SeriesId>, subset: &PostingSubset<'_>, postings: &Postings) {
        match subset {
            PostingSubset::Exact { name, value, .. } => {
                if let Some(ids) = postings
                    .entries
                    .get(*name)
                    .and_then(|entry| entry.values.get(*value))
                {
                    subtract_in_place(keep, ids);
                }
            }

            PostingSubset::Any { name, .. } => {
                if let Some(entry) = postings.entries.get(*name) {
                    subtract_in_place(keep, &entry.any);
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum PostingSubset<'a> {
    Exact {
        name: &'a str,
        value: &'a str,
        size_estimate: usize,
    },
    Any {
        name: &'a str,
        size_estimate: usize,
    },
}

impl<'a> PostingSubset<'a> {
    fn new_exact(name: &'a str, value: &'a str, postings: &Postings) -> Self {
        let size_estimate = postings
            .entries
            .get(name)
            .and_then(|entry| entry.values.get(value))
            .map_or(0, Vec::len);

        Self::Exact {
            name,
            value,
            size_estimate,
        }
    }

    fn new_any(name: &'a str, postings: &Postings) -> Self {
        let size_estimate = postings
            .entries
            .get(name)
            .map_or(0, |entry| entry.any.len());

        Self::Any {
            name,
            size_estimate,
        }
    }

    #[inline]
    fn estimated_size(&self) -> usize {
        match self {
            Self::Exact { size_estimate, .. } | Self::Any { size_estimate, .. } => *size_estimate,
        }
    }
}
