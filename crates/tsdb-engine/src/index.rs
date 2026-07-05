use std::borrow::Cow;
use std::collections::HashMap;

use tsdb_core::{
    LabelSet, Matcher, PostingLookup, SeriesId, SeriesIndex, intersect_in_place, union_all,
};

#[derive(Default)]
pub struct Index {
    inverted: HashMap<LabelSet, SeriesId>,
    forward: HashMap<SeriesId, LabelSet>,
    posting_index: HashMap<String, HashMap<String, Vec<SeriesId>>>,
    all_ids: Vec<SeriesId>,
    next_id: SeriesId,
}

impl Index {
    pub fn new() -> Self {
        Self::default()
    }
}

impl PostingLookup for Index {
    fn values_for(&self, name: &str) -> Option<&HashMap<String, Vec<SeriesId>>> {
        self.posting_index.get(name)
    }

    fn universe(&self) -> &[SeriesId] {
        &self.all_ids
    }
}

impl SeriesIndex for Index {
    fn encode(&mut self, labels: &LabelSet) -> SeriesId {
        if let Some(&id) = self.inverted.get(labels) {
            return id;
        }

        let id = self.next_id;
        self.next_id.0 += 1;

        self.inverted.insert(labels.clone(), id);
        self.forward.insert(id, labels.clone());
        self.all_ids.push(id);

        for (name, value) in labels {
            self.posting_index
                .entry(name.clone())
                .or_default()
                .entry(value.clone())
                .or_default()
                .push(id);
        }

        id
    }

    fn resolve(&self, matchers: &[Matcher]) -> Vec<SeriesId> {
        if matchers.is_empty() {
            return self.all_ids.clone();
        }

        let mut iter = matchers.iter();
        let mut candidates: Cow<[SeriesId]> = iter.next().unwrap().candidates(self);

        for m in iter {
            let this_set = m.candidates(self);
            intersect_in_place(candidates.to_mut(), &this_set);
            if candidates.is_empty() {
                return Vec::new();
            }
        }

        candidates.into_owned()
    }

    fn labels_for(&self, id: SeriesId) -> Option<LabelSet> {
        self.forward.get(&id).cloned()
    }

    fn including_label(&self, label_name: &str) -> Vec<SeriesId> {
        self.posting_index
            .get(label_name)
            .map(union_all)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tsdb_core::MatcherOperator;

    fn label_set(pairs: &[(&str, &str)]) -> LabelSet {
        let mut set = LabelSet::new();
        for (name, value) in pairs {
            set.insert(*name, *value);
        }
        set
    }

    fn not_equal(name: &str, value: &str) -> Matcher {
        Matcher::new(name, value, MatcherOperator::NotEqual)
    }

    #[test]
    fn encode_assigns_new_id_for_new_labels() {
        let mut index = Index::new();
        let ls = label_set(&[("__name__", "cpu"), ("host", "a")]);

        let id = index.encode(&ls);

        assert_eq!(index.inverted.len(), 1);
        assert_eq!(index.inverted.get(&ls), Some(&id));
    }

    #[test]
    fn encode_returns_same_id_for_same_labels() {
        let mut index = Index::new();
        let ls = label_set(&[("__name__", "cpu"), ("host", "a")]);

        let id1 = index.encode(&ls);
        let id2 = index.encode(&ls);

        assert_eq!(id1, id2);
        assert_eq!(index.inverted.len(), 1);
    }

    #[test]
    fn encode_treats_different_insertion_order_as_same_series() {
        let mut index = Index::new();

        let mut ls_a = LabelSet::new();
        ls_a.insert("__name__", "cpu");
        ls_a.insert("host", "a");

        let mut ls_b = LabelSet::new();
        ls_b.insert("host", "a");
        ls_b.insert("__name__", "cpu");

        let id_a = index.encode(&ls_a);
        let id_b = index.encode(&ls_b);

        assert_eq!(id_a, id_b);
        assert_eq!(index.inverted.len(), 1);
    }

    #[test]
    fn encode_assigns_different_ids_for_different_labels() {
        let mut index = Index::new();
        let ls_a = label_set(&[("__name__", "cpu"), ("host", "a")]);
        let ls_b = label_set(&[("__name__", "cpu"), ("host", "b")]);

        let id_a = index.encode(&ls_a);
        let id_b = index.encode(&ls_b);

        assert_ne!(id_a, id_b);
        assert_eq!(index.inverted.len(), 2);
    }

    #[test]
    fn encode_increments_ids_sequentially() {
        let mut index = Index::new();
        let ls_a = label_set(&[("host", "a")]);
        let ls_b = label_set(&[("host", "b")]);

        let id_a = index.encode(&ls_a);
        let id_b = index.encode(&ls_b);

        assert_eq!(id_b.0, id_a.0 + 1);
    }

    #[test]
    fn encode_maintains_sorted_posting_lists_and_universe() {
        let mut index = Index::new();
        let ls_a = label_set(&[("host", "a")]);
        let ls_b = label_set(&[("host", "a")]);
        let id_a = index.encode(&ls_a);
        let id_b = index.encode(&ls_b); // duplicate labels -> same id, no new entries

        assert_eq!(id_a, id_b);
        assert_eq!(index.all_ids, vec![id_a]);
        assert_eq!(
            index.posting_index.get("host").unwrap().get("a").unwrap(),
            &vec![id_a]
        );
    }

    #[test]
    fn resolve_equal_matches_single_matcher() {
        let mut index = Index::new();
        let ls_a = label_set(&[("__name__", "cpu"), ("host", "a")]);
        let ls_b = label_set(&[("__name__", "mem"), ("host", "a")]);
        let id_a = index.encode(&ls_a);
        index.encode(&ls_b);

        let result = index.resolve(&[Matcher::equal("__name__", "cpu")]);

        assert_eq!(result, vec![id_a]);
    }

    #[test]
    fn resolve_equal_matches_multiple_matchers_as_and() {
        let mut index = Index::new();
        let ls_a = label_set(&[("__name__", "cpu"), ("host", "a")]);
        let ls_b = label_set(&[("__name__", "cpu"), ("host", "b")]);
        let id_a = index.encode(&ls_a);
        index.encode(&ls_b);

        let result = index.resolve(&[
            Matcher::equal("__name__", "cpu"),
            Matcher::equal("host", "a"),
        ]);

        assert_eq!(result, vec![id_a]);
    }

    #[test]
    fn resolve_returns_empty_when_value_never_indexed() {
        let mut index = Index::new();
        let ls = label_set(&[("__name__", "cpu")]);
        index.encode(&ls);

        let result = index.resolve(&[Matcher::equal("__name__", "mem")]);

        assert!(result.is_empty());
    }

    #[test]
    fn resolve_not_equal_matches_different_value() {
        let mut index = Index::new();
        let ls_a = label_set(&[("host", "a")]);
        let ls_b = label_set(&[("host", "b")]);
        let id_a = index.encode(&ls_a);
        let id_b = index.encode(&ls_b);

        let mut result = index.resolve(&[not_equal("host", "a")]);
        result.sort_by_key(|id| id.0);

        assert_eq!(result, vec![id_b]);
        assert!(!result.contains(&id_a));
    }

    #[test]
    fn resolve_not_equal_includes_series_missing_the_label() {
        let mut index = Index::new();
        let ls_a = label_set(&[("host", "a")]);
        let ls_b = label_set(&[("__name__", "cpu")]); // no "host" label at all
        let id_a = index.encode(&ls_a);
        let id_b = index.encode(&ls_b);

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
        let id_a = index.encode(&ls_a);
        index.encode(&ls_b);

        let result = index.resolve(&[not_equal("host", "")]);

        assert_eq!(result, vec![id_a]);
    }

    #[test]
    fn resolve_with_no_matchers_returns_all_series() {
        let mut index = Index::new();
        let ls_a = label_set(&[("__name__", "cpu")]);
        let ls_b = label_set(&[("__name__", "mem")]);
        let id_a = index.encode(&ls_a);
        let id_b = index.encode(&ls_b);

        let result = index.resolve(&[]);

        assert_eq!(result.len(), 2);
        assert!(result.contains(&id_a));
        assert!(result.contains(&id_b));
    }

    #[test]
    fn labels_for_returns_labels_for_known_id() {
        let mut index = Index::new();
        let ls = label_set(&[("__name__", "cpu"), ("host", "a")]);
        let id = index.encode(&ls);

        assert_eq!(index.labels_for(id), Some(ls));
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
        let id_a = index.encode(&ls_a);
        let id_b = index.encode(&ls_b);

        let mut result = index.including_label("host");
        result.sort_by_key(|id| id.0);

        assert_eq!(result, vec![id_a, id_b]);
    }

    #[test]
    fn including_label_returns_empty_when_no_series_have_label() {
        let mut index = Index::new();
        let ls = label_set(&[("__name__", "cpu")]);
        index.encode(&ls);

        assert!(index.including_label("nonexistent").is_empty());
    }
}
