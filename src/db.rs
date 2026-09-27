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

#[derive(Default)]
struct DbState {
    store: MemTable,
    index: Index,
}

#[derive(Default)]
pub struct Db {
    state: RwLock<DbState>,
}

impl Db {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a batch while holding one write lock over the index and samples.
    /// Series with no samples are ignored and do not create index entries.
    pub fn write(&self, batch: WriteBatch) -> Result<(), DbError> {
        let mut state = self.state.write().unwrap();
        for (labels, samples) in batch.series {
            if samples.is_empty() {
                continue;
            }
            let id = state.index.encode(&labels);
            for s in samples {
                state.store.append(id, s)?;
            }
        }
        Ok(())
    }

    pub fn query(
        &self,
        matchers: &[Matcher],
        range: TimeRange,
    ) -> Result<Vec<SeriesResult>, DbError> {
        let state = self.state.read().unwrap();

        let mut out = Vec::new();
        for id in state.index.resolve(matchers) {
            let samples = state.store.read(id, range)?;
            if let Some(labels) = state.index.labels_for(id) {
                out.push(SeriesResult { labels, samples });
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;
    use std::thread;

    use super::*;
    use crate::model::Label;

    fn labels(host: &str) -> LabelSet {
        LabelSet::from_labels([Label::new("host", host)])
    }

    #[test]
    fn empty_writes_do_not_create_series() {
        let db = Db::new();
        db.write(WriteBatch { series: vec![] }).unwrap();
        db.write(WriteBatch {
            series: vec![(labels("empty"), vec![])],
        })
        .unwrap();

        assert!(
            db.query(&[], TimeRange::new(0, u64::MAX))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn empty_series_do_not_change_existing_or_mixed_batch_data() {
        let db = Db::new();
        let samples = vec![Sample::new(100, 1.0), Sample::new(200, 2.0)];
        db.write(WriteBatch {
            series: vec![
                (labels("empty"), vec![]),
                (labels("cpu"), samples.clone()),
                (labels("cpu"), vec![]),
            ],
        })
        .unwrap();
        db.write(WriteBatch {
            series: vec![(labels("cpu"), vec![])],
        })
        .unwrap();

        assert_eq!(
            db.query(&[], TimeRange::new(0, u64::MAX)).unwrap(),
            vec![SeriesResult {
                labels: labels("cpu"),
                samples,
            }]
        );
    }

    #[test]
    fn concurrent_queries_observe_complete_write_batches() {
        let db = Db::new();
        let start = Barrier::new(3);
        let samples = vec![Sample::new(100, 1.0), Sample::new(200, 2.0)];

        thread::scope(|scope| {
            scope.spawn(|| {
                start.wait();
                for batch in 0..256 {
                    db.write(WriteBatch {
                        series: (0..4)
                            .map(|series| (labels(&format!("{batch}-{series}")), samples.clone()))
                            .collect(),
                    })
                    .unwrap();
                    thread::yield_now();
                }
            });

            for _ in 0..2 {
                scope.spawn(|| {
                    start.wait();
                    for _ in 0..256 {
                        let result = db.query(&[], TimeRange::new(0, u64::MAX)).unwrap();
                        assert_eq!(result.len() % 4, 0, "partially visible write batch");
                        for series in result {
                            assert_eq!(series.samples, samples);
                        }
                        thread::yield_now();
                    }
                });
            }
        });

        let result = db.query(&[], TimeRange::new(0, u64::MAX)).unwrap();
        assert_eq!(result.len(), 1024);
        assert!(result.iter().all(|series| series.samples == samples));
    }
}
