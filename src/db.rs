use std::sync::{Mutex, RwLock};

use crate::model::{LabelSet, Matcher, Sample, TimeRange};
use crate::storage::{Index, MemTable, StorageError, Wal, WalConfig, WalError, WalRecord};
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

    #[error(transparent)]
    Wal(#[from] WalError),
}

pub struct Db {
    store: RwLock<MemTable>,
    index: RwLock<Index>,
    wal: Mutex<Wal>,
}

impl Db {
    pub fn open(wal_config: WalConfig) -> Result<Self, DbError> {
        let (wal, records) = Wal::open_or_create(wal_config)?;

        let mut series = Vec::new();
        let mut sample_records = Vec::new();

        for record in records {
            match record {
                WalRecord::Series(record) => {
                    series.push((record.id, record.labels));
                }

                WalRecord::Samples(record) => {
                    sample_records.push(record);
                }
            }
        }

        let index = Index::seeded(series)?;
        let mut store = MemTable::new();

        for record in sample_records {
            for sample in record.samples {
                store.append(record.id, sample)?;
            }
        }

        Ok(Self {
            store: RwLock::new(store),
            index: RwLock::new(index),
            wal: Mutex::new(wal),
        })
    }

    // pub fn new() -> Self {
    //     Self {
    //         store: RwLock::new(MemTable::new()),
    //         index: RwLock::new(Index::new()),
    //     }
    // }

    pub fn write(&self, batch: WriteBatch) -> Result<(), DbError> {
        for (labels, samples) in batch.series {
            let (id, is_new) = self.index.write().unwrap().encode(&labels);

            {
                let mut wal = self.wal.lock().unwrap();

                if is_new {
                    wal.append_series(id, &labels)?;
                }

                if !samples.is_empty() {
                    wal.append_samples(id, &samples)?;
                }
            }

            let mut store = self.store.write().unwrap();

            for sample in samples {
                store.append(id, sample)?;
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

// impl Default for Db {
//     fn default() -> Self {
//         Self::new()
//     }
// }
