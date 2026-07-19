use crate::db::{DbError, SeriesResult, WriteBatch};
use crate::model::{LabelSet, Matcher, TimeRange};
use crate::storage::indexes::concurrent_index::ConcurrentIndex;

const BATCH_THRESHOLD: usize = 16;

pub struct BatchedIndexDb {
    index: ConcurrentIndex,
}

impl BatchedIndexDb {
    pub fn new() -> Self {
        Self {
            index: ConcurrentIndex::new(),
        }
    }

    pub fn write(&self, batch: WriteBatch) -> Result<(), DbError> {
        if batch.series.len() <= BATCH_THRESHOLD {
            for (labels, _s) in batch.series {
                self.index.encode(&labels);
            }
            return Ok(());
        }
        let labels: Vec<LabelSet> = batch.series.into_iter().map(|(l, _s)| l).collect();
        let _ids = self.index.encode_batch(&labels);

        Ok(())
    }

    pub fn seed(&self, batch: WriteBatch) -> Result<(), DbError> {
        let labels: Vec<LabelSet> = batch.series.into_iter().map(|(l, _s)| l).collect();
        self.index.seed_prebuilt(&labels);
        Ok(())
    }

    pub fn query(
        &self,
        matchers: &[Matcher],
        _range: TimeRange,
    ) -> Result<Vec<SeriesResult>, DbError> {
        let mut out = Vec::new();
        for id in self.index.resolve(matchers) {
            if let Some(labels) = self.index.labels_for(id) {
                out.push(SeriesResult {
                    labels,
                    samples: Vec::new(),
                });
            }
        }
        Ok(out)
    }
}

impl Default for BatchedIndexDb {
    fn default() -> Self {
        Self::new()
    }
}
