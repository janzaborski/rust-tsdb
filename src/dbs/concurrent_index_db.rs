use crate::db::DbError;
use crate::db::SeriesResult;
use crate::db::WriteBatch;
use crate::model::{Matcher, TimeRange};
use crate::storage::MemTable;
use crate::storage::indexes::concurrent_index::ConcurrentIndex;
use std::sync::RwLock;

pub struct ConcurrentIndexDb {
    store: RwLock<MemTable>,
    index: ConcurrentIndex,
}

impl ConcurrentIndexDb {
    pub fn new() -> Self {
        Self {
            store: RwLock::new(MemTable::new()),
            index: ConcurrentIndex::new(),
        }
    }

    pub fn write(&self, batch: WriteBatch) -> Result<(), DbError> {
        for (labels, samples) in batch.series {
            let id = self.index.encode(&labels);
            // let mut store = self.store.write().unwrap();
            // for s in samples {
            //     store.append(id, s)?;
            // }
        }
        Ok(())
    }

    pub fn query(
        &self,
        matchers: &[Matcher],
        range: TimeRange,
    ) -> Result<Vec<SeriesResult>, DbError> {
        let store = self.store.read().unwrap();

        let mut out = Vec::new();
        for id in self.index.resolve(matchers) {
            // let samples = store.read(id, range)?;
            // if let Some(labels) = self.index.labels_for(id) {
            //     out.push(SeriesResult { labels, samples });
            // }
        }
        Ok(out)
    }

    pub fn seed(&self, batch: WriteBatch) -> Result<(), DbError> {
        let (labelsets, sample_groups): (Vec<_>, Vec<_>) = batch.series.into_iter().unzip();
        let ids = self.index.seed_prebuilt(&labelsets);
        let mut store = self.store.write().unwrap();
        for (id, samples) in ids.iter().zip(sample_groups) {
            for s in samples {
                store.append(*id, s)?;
            }
        }
        Ok(())
    }
}

impl Default for ConcurrentIndexDb {
    fn default() -> Self {
        Self::new()
    }
}
