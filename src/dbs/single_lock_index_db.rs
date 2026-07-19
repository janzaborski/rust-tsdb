use crate::db::DbError;
use crate::db::SeriesResult;
use crate::db::WriteBatch;
use crate::model::{Matcher, TimeRange};
use crate::storage::MemTable;
use crate::storage::indexes::simple_index::SimpleIndex;
use std::sync::RwLock;

pub struct SingleLockIndexDb {
    store: RwLock<MemTable>,
    index: RwLock<SimpleIndex>,
}

impl SingleLockIndexDb {
    pub fn new() -> Self {
        Self {
            store: RwLock::new(MemTable::new()),
            index: RwLock::new(SimpleIndex::new()),
        }
    }

    pub fn write(&self, batch: WriteBatch) -> Result<(), DbError> {
        for (labels, samples) in batch.series {
            let id = self.index.write().unwrap().encode(&labels);
            let mut store = self.store.write().unwrap();
            for s in samples {
                store.append(id, s)?;
            }
        }
        Ok(())
    }

    pub fn query(
        &self,
        matchers: &[Matcher],
        range: TimeRange,
    ) -> Result<Vec<SeriesResult>, DbError> {
        let index = self.index.read().unwrap();
        let store = self.store.read().unwrap();

        let mut out = Vec::new();
        for id in index.resolve(matchers) {
            // let samples = store.read(id, range)?;
            // if let Some(labels) = index.labels_for(id) {
            //     out.push(SeriesResult { labels, samples });
            // }
        }
        Ok(out)
    }
    pub fn seed(&self, batch: WriteBatch) -> Result<(), DbError> {
        let mut index = self.index.write().unwrap();
        // let mut store = self.store.write().unwrap();
        for (labels, samples) in batch.series {
            let id = index.encode(&labels);
            // for s in samples {
            //     store.append(id, s)?;
            // }
        }
        Ok(())
    }
}

impl Default for SingleLockIndexDb {
    fn default() -> Self {
        Self::new()
    }
}
