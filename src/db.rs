use std::sync::{Arc, RwLock};

use crate::model::{LabelSet, Matcher, Sample, TimeRange};
use crate::storage::{Index, MemTable, SampleStore, SeriesIndex, StorageError};
use thiserror::Error;

pub trait Database: Send + Sync {
    fn write(&self, batch: WriteBatch) -> Result<(), DbError>;
    fn query(&self, matchers: &[Matcher], range: TimeRange) -> Result<Vec<SeriesResult>, DbError>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct WriteBatch {
    pub series: Vec<(LabelSet, Vec<Sample>)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeriesResult {
    pub labels: LabelSet,
    pub samples: Vec<Sample>,
}

#[derive(Error, Debug)]
pub enum DbError {
    #[error(transparent)]
    Storage(#[from] StorageError),

    #[error("Invalid write batch: {0}")]
    InvalidWriteBatch(String),
}

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

pub fn new_in_memory_database() -> Arc<dyn Database> {
    Arc::new(Db::new(MemTable::new(), Index::new()))
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
