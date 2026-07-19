use arc_swap::ArcSwap;
use dashmap::DashMap;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use crate::model::{LabelSet, Matcher, MatcherOperator, SeriesId};

type PosintgIndex = HashMap<String, HashMap<String, ArcSwap<Vec<SeriesId>>>>;

#[derive(Default, Debug)]
pub struct ConcurrentIndex {
    forward: DashMap<SeriesId, LabelSet>,
    inverted: DashMap<LabelSet, SeriesId>,
    posting_index: RwLock<PosintgIndex>,
    next_id: AtomicU64,
    all_ids: RwLock<Vec<SeriesId>>,
}

impl ConcurrentIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn encode(&self, labels: &LabelSet) -> SeriesId {
        if let Some(id) = self.inverted.get(labels) {
            return *id;
        }

        match self.inverted.entry(labels.clone()) {
            dashmap::Entry::Occupied(entry) => *entry.get(),
            dashmap::Entry::Vacant(entry) => {
                let id = SeriesId(self.next_id.fetch_add(1, Ordering::AcqRel));
                entry.insert(id);
                self.forward.insert(id, labels.clone());

                let mut posting_writer = self.posting_index.write().unwrap();
                for (name, value) in labels.into_iter() {
                    let label_map = posting_writer.entry(name.clone()).or_default();
                    let arc_vec = label_map.entry(value.clone()).or_default();

                    arc_vec.rcu(|old_vec| {
                        let pos = old_vec.binary_search(&id).unwrap_or_else(|p| p);
                        let mut new_vec = Vec::with_capacity(old_vec.len() + 1);
                        new_vec.extend_from_slice(&old_vec[..pos]);
                        new_vec.push(id);
                        new_vec.extend_from_slice(&old_vec[pos..]);

                        new_vec
                    });
                }

                let mut all = self.all_ids.write().unwrap();
                let pos = all.binary_search(&id).unwrap_or_else(|p| p);
                all.insert(pos, id);
                id
            }
        }
    }

    pub fn encode_batch(&self, labelsets: &[LabelSet]) -> Vec<SeriesId> {
        let mut ids = Vec::with_capacity(labelsets.len());
        let mut i = 0;

        while i < labelsets.len() {
            match self.inverted.get(&labelsets[i]) {
                Some(id) => {
                    ids.push(*id);
                    i += 1;
                }
                None => break,
            }
        }

        if i == labelsets.len() {
            return ids;
        }

        let mut fresh: Vec<SeriesId> = Vec::new();
        let mut groups: HashMap<(String, String), Vec<SeriesId>> = HashMap::new();
        let mut posting = self.posting_index.write().unwrap();

        for ls in &labelsets[i..] {
            if let Some(id) = self.inverted.get(ls) {
                ids.push(*id);
                continue;
            }

            match self.inverted.entry(ls.clone()) {
                dashmap::Entry::Occupied(e) => ids.push(*e.get()),
                dashmap::Entry::Vacant(e) => {
                    let id = SeriesId(self.next_id.fetch_add(1, Ordering::AcqRel));
                    e.insert(id);
                    self.forward.insert(id, ls.clone());
                    ids.push(id);
                    fresh.push(id);
                    for (name, value) in ls {
                        groups
                            .entry((name.clone(), value.clone()))
                            .or_default()
                            .push(id);
                    }
                }
            }
        }

        for ((name, value), group_ids) in groups {
            let cell = posting.entry(name).or_default().entry(value).or_default();
            cell.rcu(|old| {
                let mut v = Vec::with_capacity(old.len() + group_ids.len());
                v.extend_from_slice(old);
                v.extend_from_slice(&group_ids);

                v
            });
        }

        if !fresh.is_empty() {
            self.all_ids.write().unwrap().extend(fresh);
        }

        ids
    }

    fn candidates(&self, matcher: &Matcher) -> Arc<Vec<SeriesId>> {
        match matcher.operator {
            MatcherOperator::Equal => {
                let posintg_reader = self.posting_index.read().unwrap();
                posintg_reader
                    .get(&matcher.name)
                    .and_then(|values| values.get(&matcher.value))
                    .map(|ids_swap| ids_swap.load_full())
                    .unwrap_or_default()
            }
            MatcherOperator::NotEqual => {
                let posting_reader = self.posting_index.read().unwrap();
                let values = posting_reader.get(&matcher.name);
                let with_label: Arc<Vec<SeriesId>> = values.map(union_all_swap).unwrap_or_default();
                let with_this_value: Arc<Vec<SeriesId>> = values
                    .and_then(|v| v.get(&matcher.value))
                    .map(|swap| swap.load_full())
                    .unwrap_or_default();
                let mut result = difference_sorted(&with_label, &with_this_value);

                if !matcher.value.is_empty() {
                    let all = self.all_ids.read().unwrap();
                    let without_label = difference_sorted(&all, &with_label);
                    result = union_sorted(&result, &without_label);
                }
                Arc::new(result)
            }
        }
    }

    pub fn resolve(&self, matchers: &[Matcher]) -> Vec<SeriesId> {
        if matchers.is_empty() {
            return self.all_ids.read().unwrap().clone();
        }

        let mut matchers_iter = matchers.iter();
        let m = matchers_iter.next().unwrap();
        let mut ret = self.candidates(m).deref().clone();

        for m in matchers_iter {
            let candidates = self.candidates(m);
            intersect_in_place(&mut ret, &candidates);

            if ret.is_empty() {
                return vec![];
            }
        }

        ret
    }

    pub fn labels_for(&self, id: SeriesId) -> Option<LabelSet> {
        self.forward.get(&id).map(|r| r.value().clone())
    }

    pub fn including_label(&self, label_name: &str) -> Vec<SeriesId> {
        self.posting_index
            .read()
            .unwrap()
            .get(label_name)
            .map(union_all_swap)
            .map(|a| a.deref().clone())
            .unwrap_or_default()
    }

    pub fn seed_prebuilt(&self, labelsets: &[LabelSet]) -> Vec<SeriesId> {
        debug_assert_eq!(
            self.next_id.load(Ordering::Acquire),
            0,
            "seed_prebuilt into a non-empty index"
        );

        let n = labelsets.len();
        let mut ids = Vec::with_capacity(n);
        let mut groups: HashMap<(&str, &str), Vec<SeriesId>> = HashMap::new();

        for (i, ls) in labelsets.iter().enumerate() {
            let id = SeriesId(i as u64);
            ids.push(id);
            self.forward.insert(id, ls.clone());
            self.inverted.insert(ls.clone(), id);
            for (name, value) in ls {
                groups
                    .entry((name.as_str(), value.as_str()))
                    .or_default()
                    .push(id);
            }
        }

        {
            let mut posting = self.posting_index.write().unwrap();
            for ((name, value), list) in groups {
                posting
                    .entry(name.to_string())
                    .or_default()
                    .insert(value.to_string(), ArcSwap::from_pointee(list));
            }
        }

        *self.all_ids.write().unwrap() = (0..n as u64).map(SeriesId).collect();
        self.next_id.store(n as u64, Ordering::Release);

        ids
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

fn union_all_swap(values: &HashMap<String, ArcSwap<Vec<SeriesId>>>) -> Arc<Vec<SeriesId>> {
    let merged = values
        .values()
        .map(|swap| swap.load_full())
        .fold(Vec::new(), |acc, v| union_sorted(&acc, &v));
    Arc::new(merged)
}
