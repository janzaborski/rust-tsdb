use std::sync::RwLock;

use crate::db::{DbError, SeriesResult, WriteBatch};
use crate::model::{LabelSet, Matcher, TimeRange};
use crate::storage::indexes::simple_index::SimpleIndex;

pub struct BatchedSingleLockDb {
    index: RwLock<SimpleIndex>,
}

impl BatchedSingleLockDb {
    pub fn new() -> Self {
        Self {
            index: RwLock::new(SimpleIndex::new()),
        }
    }

    pub fn write(&self, batch: WriteBatch) -> Result<(), DbError> {
        let mut index = self.index.write().unwrap();
        for (labels, _s) in batch.series {
            index.encode(&labels);
        }
        Ok(())
    }

    // Setup only: one lock, batched in-place encode (O(N)).
    pub fn seed(&self, batch: WriteBatch) -> Result<(), DbError> {
        let labels: Vec<LabelSet> = batch.series.into_iter().map(|(l, _s)| l).collect();
        let mut index = self.index.write().unwrap();
        let (_ids, _created) = index.encode_batch(&labels);
        Ok(())
    }

    pub fn query(
        &self,
        matchers: &[Matcher],
        _range: TimeRange,
    ) -> Result<Vec<SeriesResult>, DbError> {
        let index = self.index.read().unwrap();
        let mut out = Vec::new();
        for id in index.resolve(matchers) {
            if let Some(labels) = index.labels_for(id) {
                out.push(SeriesResult {
                    labels,
                    samples: Vec::new(),
                });
            }
        }
        Ok(out)
    }
}

impl Default for BatchedSingleLockDb {
    fn default() -> Self {
        Self::new()
    }
}
