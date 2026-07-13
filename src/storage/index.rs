use std::borrow::Cow;
use std::collections::HashMap;

use crate::model::{LabelSet, Matcher, MatcherOperator, SeriesId};

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

    fn candidates(&self, matcher: &Matcher) -> Cow<'_, [SeriesId]> {
        match matcher.operator {
            MatcherOperator::Equal => self
                .posting_index
                .get(&matcher.name)
                .and_then(|values| values.get(&matcher.value))
                .map(|ids| Cow::Borrowed(ids.as_slice()))
                .unwrap_or(Cow::Owned(Vec::new())),

            MatcherOperator::NotEqual => {
                let values = self.posting_index.get(&matcher.name);
                let with_label: Vec<SeriesId> = values.map(union_all).unwrap_or_default();
                let with_this_value: &[SeriesId] = values
                    .and_then(|v| v.get(&matcher.value))
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);

                let mut result = difference_sorted(&with_label, with_this_value);

                if !matcher.value.is_empty() {
                    let without_label = difference_sorted(&self.all_ids, &with_label);
                    result = union_sorted(&result, &without_label);
                }
                Cow::Owned(result)
            }
        }
    }
}

impl Index {
    pub fn encode(&mut self, labels: &LabelSet) -> SeriesId {
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

    /// On empty matchers returns all series ids.
    pub fn resolve(&self, matchers: &[Matcher]) -> Vec<SeriesId> {
        if matchers.is_empty() {
            return self.all_ids.clone();
        }

        let mut iter = matchers.iter();
        let mut candidates: Cow<[SeriesId]> = self.candidates(iter.next().unwrap());

        for m in iter {
            let this_set = self.candidates(m);
            intersect_in_place(candidates.to_mut(), &this_set);
            if candidates.is_empty() {
                return Vec::new();
            }
        }

        candidates.into_owned()
    }

    pub fn labels_for(&self, id: SeriesId) -> Option<LabelSet> {
        self.forward.get(&id).cloned()
    }

    pub fn including_label(&self, label_name: &str) -> Vec<SeriesId> {
        self.posting_index
            .get(label_name)
            .map(union_all)
            .unwrap_or_default()
    }
}

fn union_sorted(a: &[SeriesId], b: &[SeriesId]) -> Vec<SeriesId> {
    let mut result = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].0.cmp(&b[j].0) {
            std::cmp::Ordering::Less => {
                result.push(a[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                result.push(b[j]);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                result.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    result.extend_from_slice(&a[i..]);
    result.extend_from_slice(&b[j..]);
    result
}

fn difference_sorted(a: &[SeriesId], b: &[SeriesId]) -> Vec<SeriesId> {
    let mut result = Vec::with_capacity(a.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].0.cmp(&b[j].0) {
            std::cmp::Ordering::Less => {
                result.push(a[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
    }
    result.extend_from_slice(&a[i..]);
    result
}

fn intersect_in_place(a: &mut Vec<SeriesId>, b: &[SeriesId]) {
    let (mut write, mut i, mut j) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].0.cmp(&b[j].0) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                a[write] = a[i];
                write += 1;
                i += 1;
                j += 1;
            }
        }
    }
    a.truncate(write);
}

fn union_all(values: &HashMap<String, Vec<SeriesId>>) -> Vec<SeriesId> {
    values
        .values()
        .fold(Vec::new(), |acc, v| union_sorted(&acc, v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Label, MatcherOperator};

    fn label_set(pairs: &[(&str, &str)]) -> LabelSet {
        let mut set = LabelSet::new();
        for (name, value) in pairs {
            set.insert_label(Label::new(*name, *value));
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
        ls_a.insert_label(Label::new("__name__", "cpu"));
        ls_a.insert_label(Label::new("host", "a"));

        let mut ls_b = LabelSet::new();
        ls_b.insert_label(Label::new("host", "a"));
        ls_b.insert_label(Label::new("__name__", "cpu"));

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

        let result = index.resolve(&[Matcher::new("__name__", "cpu", MatcherOperator::Equal)]);

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
        let id_a = index.encode(&ls_a);
        let id_b = index.encode(&ls_b);

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

    #[test]
    fn resolve_equal_and_not_equal_combined() {
        let mut index = Index::new();
        let cpu_a = index.encode(&label_set(&[("__name__", "cpu"), ("host", "a")]));
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
        let c = index.encode(&label_set(&[("host", "c")]));

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
        let id = index.encode(&LabelSet::new());

        // In the universe, no posting entries, reachable only via an empty query.
        assert_eq!(index.resolve(&[]), vec![id]);
        assert_eq!(index.labels_for(id), Some(LabelSet::new()));
        assert!(index.posting_index.is_empty());
    }

    #[test]
    fn resolve_not_equal_returns_already_sorted_result() {
        let mut index = Index::new();
        let a = index.encode(&label_set(&[("host", "a")]));
        index.encode(&label_set(&[("host", "b")]));
        let c = index.encode(&label_set(&[("host", "c")]));

        // host!=b -> [a, c]. Asserted WITHOUT pre-sorting: resolve must already
        // return ascending order, which the set algebra downstream relies on.
        let result = index.resolve(&[not_equal("host", "b")]);

        assert_eq!(result, vec![a, c]);
    }
}
