use std::collections::HashMap;
use tsdb_core::Matcher;
use tsdb_core::{LabelSet, SeriesId, SeriesIndex};

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
