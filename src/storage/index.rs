use std::cmp::{Ordering, Reverse};
use std::collections::HashMap;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::model::{LabelSet, Matcher, MatcherOperator, SeriesId};
use crate::storage::series::Series;

#[derive(Default)]
struct NameEntry {
    values: HashMap<String, Vec<SeriesId>>,
    any: Vec<SeriesId>,
}

impl NameEntry {
    fn insert(&mut self, value: &str, id: SeriesId) {
        if let Some(ids) = self.values.get_mut(value) {
            ids.push(id);
        } else {
            self.values.insert(value.to_owned(), vec![id]);
        }

        self.any.push(id);
    }
}

#[derive(Default)]
struct Postings {
    entries: HashMap<String, NameEntry>,
    all: Vec<SeriesId>,
}

impl Postings {
    fn add_series(&mut self, id: SeriesId, labels: &LabelSet) {
        for (name, value) in labels {
            if let Some(entry) = self.entries.get_mut(name) {
                entry.insert(value, id);
            } else {
                self.entries
                    .entry(name.to_owned())
                    .or_default()
                    .insert(value, id);
            }
        }

        self.all.push(id);
    }

    fn exact(&self, name: &str, value: &str) -> &[SeriesId] {
        self.entries
            .get(name)
            .and_then(|entry| entry.values.get(value))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn any(&self, name: &str) -> &[SeriesId] {
        self.entries
            .get(name)
            .map(|entry| entry.any.as_slice())
            .unwrap_or(&[])
    }
}

#[derive(Default)]
pub struct Index {
    forward: HashMap<SeriesId, Arc<Series>>,
    inverted: HashMap<Arc<LabelSet>, Arc<Series>>,
    postings: Postings,
    next_id: u64,
}

impl Index {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lookup(&self, labels: &LabelSet) -> Option<Arc<Series>> {
        self.inverted.get(labels).cloned()
    }

    pub fn encode(&mut self, labels: &LabelSet) -> Arc<Series> {
        if let Some(series) = self.lookup(labels) {
            return series;
        }

        self.create_series(labels)
    }

    fn create_series(&mut self, labels: &LabelSet) -> Arc<Series> {
        let id = SeriesId(self.next_id);
        self.next_id += 1;

        let series = Arc::new(Series::new(id, Arc::new(labels.clone())));
        self.forward.insert(id, Arc::clone(&series));
        self.inverted
            .insert(Arc::clone(&series.labels), Arc::clone(&series));
        self.postings.add_series(id, labels);

        series
    }

    pub fn get(&self, id: SeriesId) -> Option<Arc<Series>> {
        self.forward.get(&id).cloned()
    }

    pub fn labels_for(&self, id: SeriesId) -> Option<Arc<LabelSet>> {
        self.forward
            .get(&id)
            .map(|series| Arc::clone(&series.labels))
    }

    pub fn id_for(&self, labels: &LabelSet) -> Option<SeriesId> {
        self.inverted.get(labels).map(|series| series.id)
    }

    pub fn with_label(&self, name: &str) -> Vec<SeriesId> {
        self.postings.any(name).to_vec()
    }

    /// On empty matchers returns all series ids.
    pub fn resolve(&self, matchers: &[Matcher]) -> Vec<SeriesId> {
        if matchers.is_empty() {
            return self.postings.all.clone();
        }

        let mut intersectors: Vec<&[SeriesId]> = Vec::new();
        let mut excluders: Vec<&[SeriesId]> = Vec::new();

        for matcher in matchers {
            let (name, value) = (matcher.name.as_str(), matcher.value.as_str());
            match (matcher.operator, value.is_empty()) {
                (MatcherOperator::Equal, true) => excluders.push(self.postings.any(name)),
                (MatcherOperator::Equal, false) => {
                    intersectors.push(self.postings.exact(name, value))
                }
                (MatcherOperator::NotEqual, true) => intersectors.push(self.postings.any(name)),
                (MatcherOperator::NotEqual, false) => {
                    excluders.push(self.postings.exact(name, value))
                }
            }
        }

        if intersectors.iter().any(|ids| ids.is_empty()) {
            return Vec::new();
        }

        intersectors.sort_unstable_by_key(|ids| ids.len());
        excluders.sort_unstable_by_key(|ids| Reverse(ids.len()));

        let mut result = intersectors
            .first()
            .map_or_else(|| self.postings.all.clone(), |ids| ids.to_vec());

        for ids in intersectors.iter().skip(1) {
            intersect_in_place(&mut result, ids);
            if result.is_empty() {
                return result;
            }
        }

        for ids in excluders {
            subtract_in_place(&mut result, ids);
            if result.is_empty() {
                return result;
            }
        }

        result
    }
}

#[derive(Default)]
pub struct SharedIndex {
    inner: RwLock<Index>,
}

impl From<Index> for SharedIndex {
    fn from(index: Index) -> Self {
        Self {
            inner: RwLock::new(index),
        }
    }
}

impl SharedIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lookup(&self, labels: &LabelSet) -> Option<Arc<Series>> {
        self.read().lookup(labels)
    }

    pub fn encode(&self, labels: &LabelSet) -> Arc<Series> {
        if let Some(series) = self.lookup(labels) {
            return series;
        }

        self.write().encode(labels)
    }

    pub fn get(&self, id: SeriesId) -> Option<Arc<Series>> {
        self.read().get(id)
    }

    pub fn labels_for(&self, id: SeriesId) -> Option<Arc<LabelSet>> {
        self.read().labels_for(id)
    }

    pub fn id_for(&self, labels: &LabelSet) -> Option<SeriesId> {
        self.read().id_for(labels)
    }

    pub fn with_label(&self, name: &str) -> Vec<SeriesId> {
        self.read().with_label(name)
    }

    pub fn resolve(&self, matchers: &[Matcher]) -> Vec<SeriesId> {
        self.read().resolve(matchers)
    }

    pub fn series(&self, matchers: &[Matcher]) -> Vec<Arc<Series>> {
        let index = self.read();
        index
            .resolve(matchers)
            .into_iter()
            .filter_map(|id| index.get(id))
            .collect()
    }

    pub(crate) fn catalog(&self, label: Option<&str>) -> Vec<String> {
        let index = self.read();
        let entries = &index.postings.entries;
        match label {
            None => entries.keys().cloned().collect(),
            Some(label) => entries
                .get(label)
                .map(|entry| entry.values.keys().cloned().collect())
                .unwrap_or_default(),
        }
    }

    fn read(&self) -> RwLockReadGuard<'_, Index> {
        self.inner.read().unwrap()
    }

    fn write(&self) -> RwLockWriteGuard<'_, Index> {
        self.inner.write().unwrap()
    }
}

fn intersect_in_place(a: &mut Vec<SeriesId>, b: &[SeriesId]) {
    let (mut write, mut i, mut j) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            Ordering::Less => i += 1,
            Ordering::Greater => j += 1,
            Ordering::Equal => {
                a[write] = a[i];
                write += 1;
                i += 1;
                j += 1;
            }
        }
    }
    a.truncate(write);
}

fn subtract_in_place(a: &mut Vec<SeriesId>, b: &[SeriesId]) {
    let (mut write, mut i, mut j) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            Ordering::Less => {
                a[write] = a[i];
                write += 1;
                i += 1;
            }
            Ordering::Greater => j += 1,
            Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
    }
    let tail = a.len() - i;
    a.copy_within(i.., write);
    a.truncate(write + tail);
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::model::MatcherOperator;

    fn label_set(pairs: &[(&str, &str)]) -> LabelSet {
        LabelSet::from_pairs(pairs.iter().copied())
    }

    fn not_equal(name: &str, value: &str) -> Matcher {
        Matcher::new(name, value, MatcherOperator::NotEqual)
    }

    #[test]
    fn encode_assigns_new_id_for_new_labels() {
        let mut index = Index::new();
        let ls = label_set(&[("__name__", "cpu"), ("host", "a")]);

        let id = index.encode(&ls).id;

        assert_eq!(index.inverted.len(), 1);
        assert_eq!(index.inverted.get(&ls).map(|series| series.id), Some(id));
    }

    #[test]
    fn encode_returns_same_id_for_same_labels() {
        let mut index = Index::new();
        let ls = label_set(&[("__name__", "cpu"), ("host", "a")]);

        let id1 = index.encode(&ls).id;
        let id2 = index.encode(&ls).id;

        assert_eq!(id1, id2);
        assert_eq!(index.inverted.len(), 1);
    }

    #[test]
    fn encode_treats_different_insertion_order_as_same_series() {
        let mut index = Index::new();

        let ls_a = label_set(&[("__name__", "cpu"), ("host", "a")]);
        let ls_b = label_set(&[("host", "a"), ("__name__", "cpu")]);

        let id_a = index.encode(&ls_a).id;
        let id_b = index.encode(&ls_b).id;

        assert_eq!(id_a, id_b);
        assert_eq!(index.inverted.len(), 1);
    }

    #[test]
    fn encode_assigns_different_ids_for_different_labels() {
        let mut index = Index::new();
        let ls_a = label_set(&[("__name__", "cpu"), ("host", "a")]);
        let ls_b = label_set(&[("__name__", "cpu"), ("host", "b")]);

        let id_a = index.encode(&ls_a).id;
        let id_b = index.encode(&ls_b).id;

        assert_ne!(id_a, id_b);
        assert_eq!(index.inverted.len(), 2);
    }

    #[test]
    fn encode_increments_ids_sequentially() {
        let mut index = Index::new();
        let ls_a = label_set(&[("host", "a")]);
        let ls_b = label_set(&[("host", "b")]);

        let id_a = index.encode(&ls_a).id;
        let id_b = index.encode(&ls_b).id;

        assert_eq!(id_b.0, id_a.0 + 1);
    }

    #[test]
    fn encode_maintains_sorted_posting_lists_and_universe() {
        let mut index = Index::new();
        let ls_a = label_set(&[("host", "a")]);
        let ls_b = label_set(&[("host", "a")]);
        let id_a = index.encode(&ls_a).id;
        let id_b = index.encode(&ls_b).id; // duplicate labels -> same id, no new entries

        assert_eq!(id_a, id_b);
        assert_eq!(index.postings.all, vec![id_a]);
        assert_eq!(
            index
                .postings
                .entries
                .get("host")
                .unwrap()
                .values
                .get("a")
                .unwrap(),
            &vec![id_a]
        );
    }

    #[test]
    fn resolve_equal_matches_single_matcher() {
        let mut index = Index::new();
        let ls_a = label_set(&[("__name__", "cpu"), ("host", "a")]);
        let ls_b = label_set(&[("__name__", "mem"), ("host", "a")]);
        let id_a = index.encode(&ls_a).id;
        index.encode(&ls_b);

        let result = index.resolve(&[Matcher::new("__name__", "cpu", MatcherOperator::Equal)]);

        assert_eq!(result, vec![id_a]);
    }

    #[test]
    fn resolve_equal_matches_multiple_matchers_as_and() {
        let mut index = Index::new();
        let ls_a = label_set(&[("__name__", "cpu"), ("host", "a")]);
        let ls_b = label_set(&[("__name__", "cpu"), ("host", "b")]);
        let id_a = index.encode(&ls_a).id;
        index.encode(&ls_b);

        let result = index.resolve(&[
            Matcher::new("__name__", "cpu", MatcherOperator::Equal),
            Matcher::new("host", "a", MatcherOperator::Equal),
        ]);

        assert_eq!(result, vec![id_a]);
    }

    #[test]
    fn resolve_returns_empty_when_value_never_indexed() {
        let mut index = Index::new();
        let ls = label_set(&[("__name__", "cpu")]);
        index.encode(&ls);

        let result = index.resolve(&[Matcher::new("__name__", "mem", MatcherOperator::Equal)]);

        assert!(result.is_empty());
    }

    #[test]
    fn resolve_not_equal_matches_different_value() {
        let mut index = Index::new();
        let ls_a = label_set(&[("host", "a")]);
        let ls_b = label_set(&[("host", "b")]);
        let id_a = index.encode(&ls_a).id;
        let id_b = index.encode(&ls_b).id;

        let mut result = index.resolve(&[Matcher::new("host", "a", MatcherOperator::NotEqual)]);
        result.sort_by_key(|id| id.0);

        assert_eq!(result, vec![id_b]);
        assert!(!result.contains(&id_a));
    }

    #[test]
    fn resolve_not_equal_includes_series_missing_the_label() {
        let mut index = Index::new();
        let ls_a = label_set(&[("host", "a")]);
        let ls_b = label_set(&[("__name__", "cpu")]); // no "host" label at all
        let id_a = index.encode(&ls_a).id;
        let id_b = index.encode(&ls_b).id;

        let mut result = index.resolve(&[not_equal("host", "a")]);
        result.sort_by_key(|id| id.0);

        assert_eq!(result, vec![id_b]);
        assert!(!result.contains(&id_a));
    }

    #[test]
    fn resolve_not_equal_against_empty_string_excludes_missing_label() {
        let mut index = Index::new();
        let ls_a = label_set(&[("host", "a")]);
        let ls_b = label_set(&[("__name__", "cpu")]); // no "host" label -> treated as ""
        let id_a = index.encode(&ls_a).id;
        index.encode(&ls_b);

        let result = index.resolve(&[not_equal("host", "")]);

        assert_eq!(result, vec![id_a]);
    }

    #[test]
    fn resolve_with_no_matchers_returns_all_series() {
        let mut index = Index::new();
        let ls_a = label_set(&[("__name__", "cpu")]);
        let ls_b = label_set(&[("__name__", "mem")]);
        let id_a = index.encode(&ls_a).id;
        let id_b = index.encode(&ls_b).id;

        let result = index.resolve(&[]);

        assert_eq!(result.len(), 2);
        assert!(result.contains(&id_a));
        assert!(result.contains(&id_b));
    }

    #[test]
    fn labels_for_returns_labels_for_known_id() {
        let mut index = Index::new();
        let ls = label_set(&[("__name__", "cpu"), ("host", "a")]);
        let id = index.encode(&ls).id;

        assert_eq!(index.labels_for(id), Some(Arc::new(ls)));
    }

    #[test]
    fn labels_for_returns_none_for_unknown_id() {
        let index = Index::new();
        assert_eq!(index.labels_for(SeriesId(999)), None);
    }

    #[test]
    fn including_label_returns_series_across_multiple_values() {
        let mut index = Index::new();
        let ls_a = label_set(&[("host", "a")]);
        let ls_b = label_set(&[("host", "b")]);
        let id_a = index.encode(&ls_a).id;
        let id_b = index.encode(&ls_b).id;

        let mut result = index.with_label("host");
        result.sort_by_key(|id| id.0);

        assert_eq!(result, vec![id_a, id_b]);
    }

    #[test]
    fn including_label_returns_empty_when_no_series_have_label() {
        let mut index = Index::new();
        let ls = label_set(&[("__name__", "cpu")]);
        index.encode(&ls);

        assert!(index.with_label("nonexistent").is_empty());
    }

    #[test]
    fn resolve_equal_and_not_equal_combined() {
        let mut index = Index::new();
        let cpu_a = index
            .encode(&label_set(&[("__name__", "cpu"), ("host", "a")]))
            .id;
        index.encode(&label_set(&[("__name__", "cpu"), ("host", "b")]));
        index.encode(&label_set(&[("__name__", "mem"), ("host", "a")]));

        // __name__=cpu AND host!=b -> cpu_a only. Equal first (borrowed Cow).
        let result = index.resolve(&[
            Matcher::new("__name__", "cpu", MatcherOperator::Equal),
            not_equal("host", "b"),
        ]);
        assert_eq!(result, vec![cpu_a]);

        // Same query, matchers reversed -> NotEqual first (owned Cow). Same result.
        let result = index.resolve(&[
            not_equal("host", "b"),
            Matcher::new("__name__", "cpu", MatcherOperator::Equal),
        ]);
        assert_eq!(result, vec![cpu_a]);
    }

    #[test]
    fn resolve_not_equal_and_not_equal_combined() {
        let mut index = Index::new();
        index.encode(&label_set(&[("host", "a")]));
        index.encode(&label_set(&[("host", "b")]));
        let c = index.encode(&label_set(&[("host", "c")])).id;

        // host!=a AND host!=b -> c
        let result = index.resolve(&[not_equal("host", "a"), not_equal("host", "b")]);

        assert_eq!(result, vec![c]);
    }

    #[test]
    fn resolve_returns_empty_when_matchers_have_no_overlap() {
        let mut index = Index::new();
        index.encode(&label_set(&[("__name__", "cpu"), ("host", "a")]));

        // First matcher matches, second matches nothing -> intersection empties.
        let result = index.resolve(&[
            Matcher::new("__name__", "cpu", MatcherOperator::Equal),
            Matcher::new("host", "nonexistent", MatcherOperator::Equal),
        ]);

        assert!(result.is_empty());
    }

    #[test]
    fn resolve_two_equal_matchers_on_same_label_is_empty() {
        let mut index = Index::new();
        index.encode(&label_set(&[("host", "a")]));
        index.encode(&label_set(&[("host", "b")]));

        // A series can't have host=a and host=b at once.
        let result = index.resolve(&[
            Matcher::new("host", "a", MatcherOperator::Equal),
            Matcher::new("host", "b", MatcherOperator::Equal),
        ]);

        assert!(result.is_empty());
    }

    #[test]
    fn resolve_on_empty_index_returns_empty() {
        let index = Index::new();

        assert!(index.resolve(&[]).is_empty());
        assert!(
            index
                .resolve(&[Matcher::new("host", "a", MatcherOperator::Equal)])
                .is_empty()
        );
        assert!(index.resolve(&[not_equal("host", "a")]).is_empty());
    }

    #[test]
    fn encode_empty_label_set_creates_label_less_series() {
        let mut index = Index::new();
        let id = index.encode(&LabelSet::default()).id;

        // In the universe, no posting entries, reachable only via an empty query.
        assert_eq!(index.resolve(&[]), vec![id]);
        assert_eq!(index.labels_for(id), Some(Arc::new(LabelSet::default())));
        assert!(index.postings.entries.is_empty());
    }

    #[test]
    fn resolve_not_equal_returns_already_sorted_result() {
        let mut index = Index::new();
        let a = index.encode(&label_set(&[("host", "a")])).id;
        index.encode(&label_set(&[("host", "b")]));
        let c = index.encode(&label_set(&[("host", "c")])).id;

        // host!=b -> [a, c]. Asserted WITHOUT pre-sorting: resolve must already
        // return ascending order, which the set algebra downstream relies on.
        let result = index.resolve(&[not_equal("host", "b")]);

        assert_eq!(result, vec![a, c]);
    }
}
