use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;

use crate::model::{LabelSet, Matcher, Sample, SeriesId, TimeRange};
use crate::storage::segment::SegmentStore;
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

    #[error("Invalid time range: start ({start}) must not exceed end ({end})")]
    InvalidTimeRange { start: u64, end: u64 },

    #[error("max_buffer_samples must be greater than zero")]
    InvalidBufferSize,

    #[error("database stopped after a storage failure; restart required")]
    StorageFailed,
}

#[derive(Default)]
struct DbState {
    store: MemTable,
    index: Index,
    disk: Option<SegmentStore>,
    latest: HashMap<SeriesId, u64>,
    buffered_samples: usize,
    max_buffer_samples: usize,
    failed: bool,
}

pub struct Db {
    state: RwLock<DbState>,
}

impl Default for Db {
    fn default() -> Self {
        Self {
            state: RwLock::new(DbState {
                max_buffer_samples: usize::MAX,
                ..DbState::default()
            }),
        }
    }
}

impl Db {
    /// Create a new in-memory database with no persistence.
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a database with persistent storage in the given directory.
    pub fn open(path: impl AsRef<Path>, max_buffer_samples: usize) -> Result<Self, DbError> {
        if max_buffer_samples == 0 {
            return Err(DbError::InvalidBufferSize);
        }
        let mut index = Index::new();
        let (disk, latest) = SegmentStore::open(path.as_ref(), &mut index)?;
        Ok(Self {
            state: RwLock::new(DbState {
                index,
                disk: Some(disk),
                latest,
                max_buffer_samples,
                ..DbState::default()
            }),
        })
    }

    pub fn write(&self, batch: WriteBatch) -> Result<(), DbError> {
        let mut state = self.state.write().unwrap();
        if state.failed {
            return Err(DbError::StorageFailed);
        }
        for (labels, samples) in batch.series {
            if samples.is_empty() {
                continue;
            }
            let id = state.index.encode(&labels);
            for s in samples {
                if state
                    .latest
                    .get(&id)
                    .is_some_and(|last| s.timestamp <= *last)
                {
                    continue;
                }
                state.store.append(id, s)?;
                state.latest.insert(id, s.timestamp);
                state.buffered_samples += 1;
            }
        }
        if state.buffered_samples >= state.max_buffer_samples {
            state.flush()?;
        }
        Ok(())
    }

    pub fn flush(&self) -> Result<(), DbError> {
        let mut state = self.state.write().unwrap();
        if state.failed {
            return Err(DbError::StorageFailed);
        }
        state.flush()
    }

    pub fn query(
        &self,
        matchers: &[Matcher],
        range: TimeRange,
    ) -> Result<Vec<SeriesResult>, DbError> {
        if range.start > range.end {
            return Err(DbError::InvalidTimeRange {
                start: range.start,
                end: range.end,
            });
        }
        let state = self.state.read().unwrap();
        if state.failed {
            return Err(DbError::StorageFailed);
        }

        let mut out = Vec::new();
        let ids = state.index.resolve(matchers);
        let labels: HashMap<_, _> = ids
            .iter()
            .filter_map(|id| state.index.labels_for(*id).map(|labels| (labels, *id)))
            .collect();
        let mut disk_samples = match &state.disk {
            Some(disk) => disk.read(&ids, &labels, range)?,
            None => HashMap::new(),
        };
        for id in ids {
            let mut samples = disk_samples.remove(&id).unwrap_or_default();
            if state.store.data.contains_key(&id) {
                samples.extend(state.store.read(id, range)?);
            }
            if let Some(labels) = state.index.labels_for(id) {
                out.push(SeriesResult { labels, samples });
            }
        }
        Ok(out)
    }
}

impl DbState {
    fn flush(&mut self) -> Result<(), DbError> {
        if self.buffered_samples == 0 {
            return Ok(());
        }
        let Some(disk) = &mut self.disk else {
            return Ok(());
        };
        if let Err(error) = disk.flush(&self.store, &self.index) {
            self.failed = true;
            return Err(error.into());
        }
        self.store = MemTable::new();
        self.buffered_samples = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Barrier;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::model::Label;

    fn labels(host: &str) -> LabelSet {
        LabelSet::from_labels([Label::new("host", host)])
    }

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path =
                std::env::temp_dir().join(format!("tsdb-test-{}-{nonce}", std::process::id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn flushed_segments_survive_restart_and_queries_merge_with_buffer() {
        let dir = TestDir::new();
        let db = Db::open(&dir.0, 3).unwrap();
        db.write(WriteBatch {
            series: vec![
                (
                    labels("a"),
                    vec![Sample::new(100, 1.0), Sample::new(200, 2.0)],
                ),
                (labels("b"), vec![Sample::new(150, 3.0)]),
            ],
        })
        .unwrap();
        assert!(db.state.read().unwrap().store.data.is_empty());

        db.write(WriteBatch {
            series: vec![(labels("a"), vec![Sample::new(300, 4.0)])],
        })
        .unwrap();
        assert_eq!(
            db.query(&[], TimeRange::new(150, 300)).unwrap(),
            vec![
                SeriesResult {
                    labels: labels("a"),
                    samples: vec![Sample::new(200, 2.0), Sample::new(300, 4.0)],
                },
                SeriesResult {
                    labels: labels("b"),
                    samples: vec![Sample::new(150, 3.0)],
                },
            ]
        );
        drop(db); // Simulate a crash: the unflushed sample is lost.

        let db = Db::open(&dir.0, 3).unwrap();
        db.write(WriteBatch {
            series: vec![(
                labels("a"),
                vec![Sample::new(200, 9.0), Sample::new(300, 4.0)],
            )],
        })
        .unwrap();
        db.flush().unwrap();
        drop(db);

        let db = Db::open(&dir.0, 3).unwrap();
        assert_eq!(
            db.query(&[], TimeRange::new(0, u64::MAX)).unwrap()[0].samples,
            vec![
                Sample::new(100, 1.0),
                Sample::new(200, 2.0),
                Sample::new(300, 4.0)
            ]
        );
    }

    #[test]
    fn completed_segments_are_checked_and_temporary_files_ignored() {
        let dir = TestDir::new();
        let db = Db::open(&dir.0, 1).unwrap();
        db.write(WriteBatch {
            series: vec![(labels("a"), vec![Sample::new(1, 1.0)])],
        })
        .unwrap();
        drop(db);
        fs::write(
            dir.0.join("segment-00000000000000000001.tmp"),
            b"unfinished",
        )
        .unwrap();
        let db = Db::open(&dir.0, 1).unwrap();
        assert_eq!(db.query(&[], TimeRange::new(0, 1)).unwrap().len(), 1);
        drop(db);

        let segment = dir.0.join("segment-00000000000000000000.seg");
        let mut bytes = fs::read(&segment).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        fs::write(segment, bytes).unwrap();
        assert!(matches!(
            Db::open(&dir.0, 1),
            Err(DbError::Storage(StorageError::CorruptSegment(_)))
        ));
    }

    #[test]
    fn directory_is_exclusive_and_failed_flush_stops_database() {
        let dir = TestDir::new();
        assert!(matches!(
            Db::open(&dir.0, 0),
            Err(DbError::InvalidBufferSize)
        ));
        let db = Db::open(&dir.0, 1).unwrap();
        assert!(Db::open(&dir.0, 1).is_err());

        fs::create_dir(dir.0.join("segment-00000000000000000000.tmp")).unwrap();
        assert!(
            db.write(WriteBatch {
                series: vec![(labels("a"), vec![Sample::new(1, 1.0)])],
            })
            .is_err()
        );
        assert!(matches!(
            db.query(&[], TimeRange::new(0, 1)),
            Err(DbError::StorageFailed)
        ));
        assert!(matches!(db.flush(), Err(DbError::StorageFailed)));
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
    fn query_rejects_reversed_ranges_even_without_matching_series() {
        let db = Db::new();
        let range = TimeRange::new(200, 100);
        assert!(matches!(
            db.query(&[], range),
            Err(DbError::InvalidTimeRange {
                start: 200,
                end: 100
            })
        ));

        db.write(WriteBatch {
            series: vec![(labels("cpu"), vec![Sample::new(100, 1.0)])],
        })
        .unwrap();
        assert!(matches!(
            db.query(&[], range),
            Err(DbError::InvalidTimeRange {
                start: 200,
                end: 100
            })
        ));
        assert_eq!(
            db.query(&[], TimeRange::new(100, 100)).unwrap()[0].samples,
            vec![Sample::new(100, 1.0)]
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
