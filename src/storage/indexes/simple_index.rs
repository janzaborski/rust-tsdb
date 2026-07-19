use std::borrow::Cow;
use std::collections::HashMap;

use crate::model::{LabelSet, Matcher, MatcherOperator, SeriesId};

#[derive(Default)]
pub struct SimpleIndex {
    inverted: HashMap<LabelSet, SeriesId>,
    forward: HashMap<SeriesId, LabelSet>,
    posting_index: HashMap<String, HashMap<String, Vec<SeriesId>>>,
    all_ids: Vec<SeriesId>,
    next_id: SeriesId,
}

impl SimpleIndex {
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

impl SimpleIndex {
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

    pub fn encode_batch(&mut self, labelsets: &[LabelSet]) -> (Vec<SeriesId>, Vec<usize>) {
        let mut ids = Vec::with_capacity(labelsets.len());
        let mut created = Vec::new();

        for (i, ls) in labelsets.iter().enumerate() {
            let next_before = self.next_id;
            let id = self.encode(ls);
            if id == next_before {
                created.push(i);
            }
            ids.push(id);
        }

        (ids, created)
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
