use ahash::{AHashMap, RandomState};
use parking_lot::RwLock;
use scc::HashIndex;
use smallvec::SmallVec;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::model::{Matcher, MatcherOperator, SeriesId};
use crate::packed_model::PackedLabelSet;
use crate::utils::algorithms::set_ops::{insert_sorted, intersect_in_place, subtract_in_place};

const INLINE_SUBSETS: usize = 8;

#[derive(Default)]
struct NameEntry {
    values: AHashMap<String, Vec<SeriesId>>,
    any: Vec<SeriesId>,
}

impl NameEntry {
    pub fn insert(&mut self, value: &str, id: SeriesId) {
        if let Some(ids) = self.values.get_mut(value) {
            insert_sorted(ids, id);
        } else {
            self.values.insert(value.to_owned(), vec![id]);
        }

        insert_sorted(&mut self.any, id);
    }
}

#[derive(Default)]
struct Postings {
    entries: HashIndex<String, Arc<RwLock<NameEntry>>, RandomState>,
    all: RwLock<Vec<SeriesId>>,
}

impl Postings {
    pub fn add_series(&self, id: SeriesId, labels: &Arc<PackedLabelSet>) {
        for (name, value) in labels.as_ref() {
            if let Some(entry) = self.entries.peek_with(name, |_, entry| Arc::clone(entry)) {
                entry.write().insert(value, id);
            } else {
                let hi_entry = self.entries.entry_sync(name.to_owned()).or_default();
                hi_entry.get().write().insert(value, id);
            }
        }

        let mut all = self.all.write();
        insert_sorted(&mut all, id);
    }
}

#[derive(Default)]
pub struct IndexV10APacked {
    forward: HashIndex<SeriesId, Arc<PackedLabelSet>, RandomState>,
    inverted: HashIndex<Arc<PackedLabelSet>, SeriesId, RandomState>,
    postings: Postings,
    next_id: AtomicU64,
}

impl IndexV10APacked {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn lookup(&self, labels: &PackedLabelSet) -> Option<SeriesId> {
        self.inverted.peek_with(labels, |_, &id| id)
    }

    pub fn encode(&self, labels: &PackedLabelSet) -> SeriesId {
        if let Some(id) = self.lookup(labels) {
            return id;
        }

        let shared = Arc::new(labels.clone());
        let entry = self
            .inverted
            .entry_sync(shared)
            .or_insert_with_key(|labels| self.create_series(labels));

        *entry.get()
    }

    fn create_series(&self, labels: &Arc<PackedLabelSet>) -> SeriesId {
        let raw_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let id = SeriesId(raw_id);

        self.forward
            .insert_sync(id, Arc::clone(labels))
            .expect("SeriesId is unique");
        self.postings.add_series(id, labels);

        id
    }

    #[inline]
    pub fn labels_for(&self, id: SeriesId) -> Option<Arc<PackedLabelSet>> {
        self.forward.peek_with(&id, |_, labels| Arc::clone(labels))
    }

    #[inline]
    pub fn with_label(&self, name: &str) -> Option<Vec<SeriesId>> {
        self.postings
            .entries
            .peek_with(name, |_, e| e.read().any.clone())
    }

    pub fn resolve(&self, matchers: &[Matcher]) -> Vec<SeriesId> {
        if matchers.is_empty() {
            return self.postings.all.read().clone();
        }

        let mut intersectors: SmallVec<[PostingSubset; INLINE_SUBSETS]> = SmallVec::new();
        let mut excluders: SmallVec<[PostingSubset; INLINE_SUBSETS]> = SmallVec::new();

        for matcher in matchers {
            match matcher.operator {
                MatcherOperator::Equal => {
                    if matcher.value.is_empty() {
                        let exc = PostingSubset::new_any(&matcher.name, &self.postings);
                        if exc.get_estimated_size() != 0 {
                            excluders.push(exc);
                        }
                    } else {
                        let int =
                            PostingSubset::new_exact(&matcher.name, &matcher.value, &self.postings);

                        if int.get_estimated_size() == 0 {
                            return vec![];
                        }

                        intersectors.push(int);
                    }
                }

                MatcherOperator::NotEqual => {
                    if matcher.value.is_empty() {
                        let int = PostingSubset::new_any(&matcher.name, &self.postings);

                        if int.get_estimated_size() == 0 {
                            return vec![];
                        }

                        intersectors.push(int);
                    } else {
                        let exc =
                            PostingSubset::new_exact(&matcher.name, &matcher.value, &self.postings);

                        if exc.get_estimated_size() != 0 {
                            excluders.push(exc);
                        }
                    }
                }
            }
        }

        intersectors.sort_unstable_by_key(|sub| sub.get_estimated_size());
        excluders.sort_unstable_by_key(|sub| std::cmp::Reverse(sub.get_estimated_size()));

        let mut result: Vec<SeriesId> = if intersectors.is_empty() {
            self.postings.all.read().clone()
        } else {
            let opt = match intersectors[0] {
                PostingSubset::Exact { name, value, .. } => self
                    .postings
                    .entries
                    .peek_with(name, |_, entry| entry.read().values.get(value).cloned())
                    .flatten(),
                PostingSubset::Any { name, .. } => self
                    .postings
                    .entries
                    .peek_with(name, |_, entry| entry.read().any.clone()),
            };

            if let Some(ids) = opt {
                ids
            } else {
                return vec![];
            }
        };

        for intersector in intersectors.iter().skip(1) {
            Self::intersect_with_subset(&mut result, intersector, &self.postings);
            if result.is_empty() {
                return result;
            }
        }

        for excluder in excluders {
            Self::exclude_subset(&mut result, &excluder, &self.postings);
            if result.is_empty() {
                return result;
            }
        }

        result
    }

    #[inline]
    fn intersect_with_subset(
        keep: &mut Vec<SeriesId>,
        subset: &PostingSubset,
        postings: &Postings,
    ) {
        match subset {
            PostingSubset::Exact { name, value, .. } => {
                postings.entries.peek_with(*name, |_, entry| {
                    let entry = entry.read();
                    if let Some(ids) = entry.values.get(*value) {
                        intersect_in_place(keep, ids);
                    }
                });
            }
            PostingSubset::Any { name, .. } => {
                postings.entries.peek_with(*name, |_, entry| {
                    let entry = entry.read();
                    intersect_in_place(keep, &entry.any);
                });
            }
        }
    }

    #[inline]
    fn exclude_subset(keep: &mut Vec<SeriesId>, subset: &PostingSubset, postings: &Postings) {
        match subset {
            PostingSubset::Exact { name, value, .. } => {
                postings.entries.peek_with(*name, |_, entry| {
                    let entry = entry.read();

                    if let Some(ids) = entry.values.get(*value) {
                        subtract_in_place(keep, ids);
                    }
                });
            }
            PostingSubset::Any { name, .. } => {
                postings.entries.peek_with(*name, |_, entry| {
                    let entry = entry.read();

                    subtract_in_place(keep, &entry.any);
                });
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
    // UnionMatch { name: &str, regex: &str, size_estimate: usize, },
    // UnionNonMatch { name: &str, regex: &str, size_estimate: usize, },
}

impl<'a> PostingSubset<'a> {
    fn new_exact(name: &'a str, value: &'a str, postings: &Postings) -> Self {
        PostingSubset::Exact {
            name,
            value,
            size_estimate: postings
                .entries
                .peek_with(name, |_, entry| {
                    entry.read().values.get(value).map(|ids| ids.len())
                })
                .flatten()
                .unwrap_or(0),
        }
    }

    fn new_any(name: &'a str, postings: &Postings) -> Self {
        PostingSubset::Any {
            name,
            size_estimate: postings
                .entries
                .peek_with(name, |_, entry| entry.read().any.len())
                .unwrap_or(0),
        }
    }

    #[inline]
    fn get_estimated_size(&self) -> usize {
        match self {
            Self::Exact { size_estimate, .. } | Self::Any { size_estimate, .. } => *size_estimate,
        }
    }
}
