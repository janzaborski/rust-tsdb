use std::sync::RwLock;

use crate::model::{LabelSet, Matcher, Sample, TimeRange};
use crate::storage::{Index, MemTable, StorageError};
use thiserror::Error;

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

pub struct Db {
    store: RwLock<MemTable>,
    index: RwLock<Index>,
}

impl Db {
    pub fn new() -> Self {
        Self {
            store: RwLock::new(MemTable::new()),
            index: RwLock::new(Index::new()),
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
            let samples = store.read(id, range)?;
            if let Some(labels) = index.labels_for(id) {
                out.push(SeriesResult { labels, samples });
            }
        }
        Ok(out)
    }
}

impl Default for Db {
    fn default() -> Self {
        Self::new()
    }
}
