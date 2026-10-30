use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, RwLock};

use crate::model::{LabelSet, Matcher, Sample, SeriesId, TimeRange};
use crate::storage::segment::SegmentStore;
use crate::storage::wal::{Durability, Lsn, ReplaySink, Wal, WalConfig, WalError, WalWriter};
use crate::storage::{Index, Series, StorageError};
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

    #[error(transparent)]
    Wal(#[from] WalError),
}

struct Persistence {
    disk: SegmentStore,
    wal: Wal,
}

#[derive(Default)]
struct DbState {
    index: Index,
    persistence: Option<Persistence>,
    buffered_samples: usize,
    max_buffer_samples: usize,
    failed: bool,
    generation: u64,
}

pub struct Db {
    state: RwLock<DbState>,
    wal: Option<WalWriter>,
    durability: Durability,
}

impl Default for Db {
    fn default() -> Self {
        Self {
            state: RwLock::new(DbState {
                max_buffer_samples: usize::MAX,
                generation: 1,
                ..DbState::default()
            }),
            wal: None,
            durability: Durability::None,
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
        let wal = WalConfig::new(path.as_ref().join("wal"));
        Self::open_with(path, max_buffer_samples, wal, Durability::None)
    }

    pub fn open_with(
        path: impl AsRef<Path>,
        max_buffer_samples: usize,
        wal: WalConfig,
        durability: Durability,
    ) -> Result<Self, DbError> {
        if max_buffer_samples == 0 {
            return Err(DbError::InvalidBufferSize);
        }
        let mut index = Index::new();
        let (disk, latest) = SegmentStore::open(path.as_ref(), &mut index)?;
        for (id, timestamp) in latest {
            if let Some(series) = index.get(id) {
                series.lock().restore_newest(timestamp);
            }
        }

        let mut replay = Replay {
            index: &mut index,
            buffered_samples: 0,
        };
        let wal = Wal::open(wal, &mut replay)?;
        let buffered_samples = replay.buffered_samples;
        let replayed = wal.recovery().last_segment;
        let writer = wal.writer();

        let mut state = DbState {
            index,
            persistence: Some(Persistence { disk, wal }),
            buffered_samples,
            max_buffer_samples,
            failed: false,
            generation: 1,
        };
        if let Some(last) = replayed {
            state.flush()?;
            if let Some(p) = &state.persistence {
                p.wal.remove_through(last)?;
            }
        }
        Ok(Self {
            state: RwLock::new(state),
            wal: Some(writer),
            durability,
        })
    }

    pub fn write(&self, batch: WriteBatch) -> Result<Lsn, DbError> {
        let mut state = self.state.write().unwrap();
        if state.failed {
            return Err(DbError::StorageFailed);
        }
        let generation = state.generation;
        let mut new_series: Vec<Arc<Series>> = Vec::new();
        let mut logging = HashSet::new();
        let mut entries: Vec<(Arc<Series>, Vec<Sample>)> = Vec::new();
        for (labels, samples) in batch.series {
            if samples.is_empty() {
                continue;
            }
            let series = state.index.encode(&labels);
            if !series.lock().is_logged(generation) && logging.insert(series.id) {
                new_series.push(Arc::clone(&series));
            }
            entries.push((series, samples));
        }

        let lsn = match &self.wal {
            Some(wal) => wal.append(
                new_series.iter().map(|s| (s.id, &*s.labels)),
                entries.iter().map(|(s, samples)| (s.id, &samples[..])),
            )?,
            None => Lsn::default(),
        };

        for series in &new_series {
            series.lock().mark_logged(generation);
        }

        for (series, samples) in entries {
            let mut series = series.lock();
            for s in samples {
                if series.append(s) {
                    state.buffered_samples += 1;
                }
            }
        }
        if state.buffered_samples >= state.max_buffer_samples {
            state.flush()?;
        }
        Ok(lsn)
    }

    pub async fn wait(&self, lsn: Lsn) {
        if let Some(wal) = &self.wal {
            wal.wait(lsn, self.durability).await;
        }
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
        let mut disk_samples = match &state.persistence {
            Some(p) => p.disk.read(&ids, &labels, range)?,
            None => HashMap::new(),
        };
        for id in ids {
            let mut samples = disk_samples.remove(&id).unwrap_or_default();
            if let Some(series) = state.index.get(id) {
                samples.extend_from_slice(series.lock().range(range));
                out.push(SeriesResult {
                    labels: LabelSet::clone(&series.labels),
                    samples,
                });
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
        let Some(p) = &mut self.persistence else {
            return Ok(());
        };
        let cut = p.wal.cut();
        let table: Vec<(SeriesId, Vec<Sample>)> = self
            .index
            .resolve(&[])
            .into_iter()
            .filter_map(|id| {
                let samples = self.index.get(id)?.lock().take_samples();
                (!samples.is_empty()).then_some((id, samples))
            })
            .collect();
        if let Err(error) = p.disk.flush(&table, &self.index) {
            self.failed = true;
            return Err(error.into());
        }
        self.buffered_samples = 0;
        self.generation += 1;
        p.wal.remove_through(cut)?;
        Ok(())
    }
}

struct Replay<'a> {
    index: &'a mut Index,
    buffered_samples: usize,
}

impl ReplaySink for Replay<'_> {
    fn series(&mut self, labels: LabelSet) -> SeriesId {
        self.index.encode(&labels).id
    }

    fn samples(&mut self, id: SeriesId, samples: Vec<Sample>) {
        if let Some(series) = self.index.get(id) {
            let mut series = series.lock();
            for s in samples {
                if series.append(s) {
                    self.buffered_samples += 1;
                }
            }
        }
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
        let state = db.state.read().unwrap();
        assert!(
            state.index.resolve(&[]).iter().all(|id| state
                .index
                .get(*id)
                .unwrap()
                .lock()
                .samples()
                .is_empty())
        );
        drop(state);

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
