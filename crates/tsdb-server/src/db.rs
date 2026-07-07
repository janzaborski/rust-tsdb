use std::sync::RwLock;

use tsdb_api::{Database, SeriesResult, WriteBatch};
use tsdb_core::{DbError, Matcher, SampleStore, SeriesIndex, TimeRange};
use tsdb_engine::{Index, MemTable};

#[derive(Default)]
pub struct Db {
    store: RwLock<MemTable>,
    index: RwLock<Index>,
}

impl Db {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Database for Db {
    fn write(&self, batch: WriteBatch) -> Result<(), DbError> {
        for (labels, samples) in batch.series {
            let id = self.index.write().unwrap().encode(&labels);
            let mut store = self.store.write().unwrap();
            for s in samples {
                store.append(id, s)?;
            }
        }
        Ok(())
    }

    fn query(&self, matchers: &[Matcher], range: TimeRange) -> Result<Vec<SeriesResult>, DbError> {
        let index = self.index.read().unwrap();
        let store = self.store.read().unwrap();

        let mut out = Vec::new();
        for id in index.resolve(matchers) {
            let samples = store.read(id, range)?;
            if let Some(labels) = index.labels_for(id) {
                out.push(SeriesResult { labels, samples });
            }
        }
        Ok(out)
    }
}
