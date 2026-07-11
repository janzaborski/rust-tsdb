use std::sync::RwLock;

use tsdb_api::{Database, SeriesResult, WriteBatch};
use tsdb_core::{DbError, Matcher, SampleStore, SeriesIndex, TimeRange};

pub struct Db<S, I> {
    store: RwLock<S>,
    index: RwLock<I>,
}

impl<S, I> Db<S, I> {
    pub fn new(store: S, index: I) -> Self {
        Self {
            store: RwLock::new(store),
            index: RwLock::new(index),
        }
    }
}

impl<S, I> Database for Db<S, I>
where
    S: SampleStore + Send + Sync,
    I: SeriesIndex + Send + Sync,
{
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
