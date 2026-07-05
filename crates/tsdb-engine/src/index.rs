use std::collections::HashMap;
use tsdb_core::{LabelSet, Matcher, SeriesId, SeriesIndex};

#[derive(Default)]
pub struct Index {
    index: HashMap<LabelSet, SeriesId>,
    next_id: SeriesId,
}

impl Index {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SeriesIndex for Index {
    fn encode(&mut self, labels: &LabelSet) -> SeriesId {
        if let Some(&id) = self.index.get(labels) {
            return id;
        }
        let id = self.next_id;
        self.next_id.0 += 1;
        self.index.insert(labels.clone(), id);
        id
    }

    fn resolve(&self, matchers: &[Matcher]) -> Vec<SeriesId> {
        self.index
            .iter()
            .filter(|(ls, _)| {
                matchers.iter().all(|m| {
                    ls.get(m.name.as_str())
                        .map_or_else(|| m.matches(""), |v| m.matches(v))
                })
            })
            .map(|(_, id)| *id)
            .collect()
    }

    fn labels_for(&self, id: SeriesId) -> Option<LabelSet> {
        self.index
            .iter()
            .find(|(_, tid)| **tid == id)
            .map(|(ls, _)| ls.clone())
    }

    fn including_label(&self, label_name: &str) -> Vec<SeriesId> {
        self.index
            .iter()
            .filter(|(ls, _)| ls.get(label_name).is_some())
            .map(|(_, id)| *id)
            .collect()
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

    fn eq_matcher(name: &str, value: &str) -> Matcher {
        Matcher::new(name.into(), value.into(), MatcherOperator::Equal)
    }

    #[test]
    fn encode_assigns_new_id_for_new_labels() {
        let mut index = Index::new();
        let ls = label_set(&[("__name__", "cpu"), ("host", "a")]);

        let id = index.encode(&ls);

        assert_eq!(index.index.len(), 1);
        assert_eq!(index.index.get(&ls), Some(&id));
    }

    #[test]
    fn encode_returns_same_id_for_same_labels() {
        let mut index = Index::new();
        let ls = label_set(&[("__name__", "cpu"), ("host", "a")]);

        let id1 = index.encode(&ls);
        let id2 = index.encode(&ls);

        assert_eq!(id1, id2);
        assert_eq!(index.index.len(), 1);
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
        assert_eq!(index.index.len(), 1);
    }

    #[test]
    fn encode_assigns_different_ids_for_different_labels() {
        let mut index = Index::new();
        let ls_a = label_set(&[("__name__", "cpu"), ("host", "a")]);
        let ls_b = label_set(&[("__name__", "cpu"), ("host", "b")]);

        let id_a = index.encode(&ls_a);
        let id_b = index.encode(&ls_b);

        assert_ne!(id_a, id_b);
        assert_eq!(index.index.len(), 2);
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
    fn resolve_matches_single_matcher() {
        let mut index = Index::new();
        let ls_a = label_set(&[("__name__", "cpu"), ("host", "a")]);
        let ls_b = label_set(&[("__name__", "mem"), ("host", "a")]);
        let id_a = index.encode(&ls_a);
        let _id_b = index.encode(&ls_b);

        let matchers = vec![eq_matcher("__name__", "cpu")];
        let result = index.resolve(&matchers);

        assert_eq!(result, vec![id_a]);
    }

    #[test]
    fn resolve_matches_multiple_matchers_as_and() {
        let mut index = Index::new();
        let ls_a = label_set(&[("__name__", "cpu"), ("host", "a")]);
        let ls_b = label_set(&[("__name__", "cpu"), ("host", "b")]);
        let id_a = index.encode(&ls_a);
        let _id_b = index.encode(&ls_b);

        let matchers = vec![eq_matcher("__name__", "cpu"), eq_matcher("host", "a")];
        let result = index.resolve(&matchers);

        assert_eq!(result, vec![id_a]);
    }

    #[test]
    fn resolve_returns_empty_when_nothing_matches() {
        let mut index = Index::new();
        let ls = label_set(&[("__name__", "cpu")]);
        index.encode(&ls);

        let matchers = vec![eq_matcher("__name__", "mem")];
        let result = index.resolve(&matchers);

        assert!(result.is_empty());
    }

    #[test]
    fn resolve_treats_missing_label_as_empty_string() {
        let mut index = Index::new();
        let ls = label_set(&[("__name__", "cpu")]); // no "host" label
        index.encode(&ls);

        let matchers = vec![eq_matcher("host", "a")];
        let result = index.resolve(&matchers);

        assert!(result.is_empty());
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

        let result = index.labels_for(id);

        assert_eq!(result, Some(ls));
    }

    #[test]
    fn labels_for_returns_none_for_unknown_id() {
        let index = Index::new();

        let result = index.labels_for(SeriesId(999));

        assert_eq!(result, None);
    }

    #[test]
    fn including_label_returns_series_with_label_present() {
        let mut index = Index::new();
        let ls_a = label_set(&[("__name__", "cpu"), ("host", "a")]);
        let ls_b = label_set(&[("__name__", "mem")]); // no "host"
        let id_a = index.encode(&ls_a);
        let _id_b = index.encode(&ls_b);

        let result = index.including_label("host");

        assert_eq!(result, vec![id_a]);
    }

    #[test]
    fn including_label_returns_empty_when_no_series_have_label() {
        let mut index = Index::new();
        let ls = label_set(&[("__name__", "cpu")]);
        index.encode(&ls);

        let result = index.including_label("nonexistent");

        assert!(result.is_empty());
    }
}
