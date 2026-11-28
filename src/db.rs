use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Mutex, RwLock};

use crate::model::{LabelSet, Matcher, Sample, SeriesId, TimeRange};
use crate::storage::segment::SegmentStore;
use crate::storage::wal::{Durability, Lsn, ReplaySink, Wal, WalConfig, WalError, WalWriter};
use crate::storage::{Index, Series, SharedIndex, StorageError};
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
    disk: Mutex<SegmentStore>,
    wal: Wal,
}

pub struct Db {
    index: SharedIndex,
    generation: RwLock<u64>,
    buffered_samples: AtomicUsize,
    max_buffer_samples: usize,
    failed: AtomicBool,
    persistence: Option<Persistence>,
    wal: Option<WalWriter>,
    durability: Durability,
}

impl Default for Db {
    fn default() -> Self {
        Self {
            index: SharedIndex::default(),
            generation: RwLock::new(1),
            buffered_samples: AtomicUsize::new(0),
            max_buffer_samples: 100000,
            failed: AtomicBool::new(false),
            persistence: None,
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
        let buffered_samples = AtomicUsize::new(replay.buffered_samples);
        let replayed = wal.recovery().last_segment;
        let writer = wal.writer();
        let index = SharedIndex::from(index);
        let persistence = Persistence {
            disk: Mutex::new(disk),
            wal,
        };
        let db = Self {
            index,
            generation: RwLock::new(1),
            buffered_samples,
            max_buffer_samples,
            failed: AtomicBool::new(false),
            persistence: Some(persistence),
            wal: Some(writer),
            durability,
        };
        if let Some(last) = replayed {
            db.flush()?;
            if let Some(p) = &db.persistence {
                p.wal.remove_through(last)?;
            }
        }
        Ok(db)
    }

    pub fn write(&self, batch: WriteBatch) -> Result<Lsn, DbError> {
        if self.failed.load(Ordering::Relaxed) {
            return Err(DbError::StorageFailed);
        }

        let generation = self.generation.read().unwrap();

        let mut new_series: Vec<Arc<Series>> = Vec::new();
        let mut logging = HashSet::new();
        let mut entries: Vec<(Arc<Series>, Vec<Sample>)> = Vec::new();
        for (labels, samples) in batch.series {
            if samples.is_empty() {
                continue;
            }
            let series = self.index.encode(&labels);
            if !series.lock().is_logged(*generation) && logging.insert(series.id) {
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
            series.lock().mark_logged(*generation);
        }

        let mut accepted = 0;
        for (series, samples) in entries {
            let mut series = series.lock();
            for s in samples {
                if series.append(s) {
                    accepted += 1;
                }
            }
        }

        drop(generation);

        let total = self.buffered_samples.fetch_add(accepted, Ordering::Relaxed) + accepted;
        if total >= self.max_buffer_samples {
            self.flush_if_at_least(self.max_buffer_samples)?;
        }
        Ok(lsn)
    }

    pub async fn wait(&self, lsn: Lsn) {
        if let Some(wal) = &self.wal {
            wal.wait(lsn, self.durability).await;
        }
    }

    pub fn is_ready(&self) -> bool {
        !self.failed.load(Ordering::Relaxed)
    }

    pub fn flush(&self) -> Result<(), DbError> {
        self.flush_if_at_least(1)
    }

    fn flush_if_at_least(&self, min: usize) -> Result<(), DbError> {
        let Some(p) = &self.persistence else {
            return Ok(());
        };
        let mut disk = p.disk.lock().unwrap();
        if self.failed.load(Ordering::Relaxed) {
            return Err(DbError::StorageFailed);
        }
        if self.buffered_samples.load(Ordering::Relaxed) < min {
            return Ok(());
        }

        let ticket = {
            let mut generation = self.generation.write().unwrap();
            let ticket = p.wal.mark_cut()?;
            *generation += 1;
            ticket
        };

        let mut taken = 0;
        let table: Vec<(Arc<Series>, Vec<Sample>)> = self
            .index
            .series(&[])
            .into_iter()
            .filter_map(|s| {
                let samples = s.lock().take_samples();
                taken += samples.len();
                (!samples.is_empty()).then_some((s, samples))
            })
            .collect();

        if let Err(e) = disk.flush(&table) {
            self.failed.store(true, Ordering::Relaxed);
            return Err(e.into());
        }
        self.buffered_samples.fetch_sub(taken, Ordering::Relaxed);
        let cut = p.wal.wait_cut(ticket);
        p.wal.remove_through(cut)?;

        Ok(())
    }

    pub(crate) fn catalog(&self, label: Option<&str>) -> Vec<String> {
        self.index.catalog(label)
    }

    pub(crate) fn series_labels(&self, matchers: &[Matcher]) -> Vec<Arc<LabelSet>> {
        self.index
            .series(matchers)
            .into_iter()
            .map(|series| Arc::clone(&series.labels))
            .collect()
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
        if self.failed.load(Ordering::Relaxed) {
            return Err(DbError::StorageFailed);
        }
        let series = self.index.series(matchers);
        let ids: Vec<SeriesId> = series.iter().map(|s| s.id).collect();
        let labels: HashMap<LabelSet, SeriesId> = series
            .iter()
            .map(|s| (LabelSet::clone(&s.labels), s.id))
            .collect();

        let disk = self.persistence.as_ref().map(|p| p.disk.lock().unwrap());
        let mut disk_samples = match &disk {
            Some(disk) => disk.read(&ids, &labels, range)?,
            None => HashMap::new(),
        };

        let mut out = Vec::with_capacity(series.len());
        for series in series {
            let mut samples = disk_samples.remove(&series.id).unwrap_or_default();
            samples.extend_from_slice(series.lock().range(range));
            out.push(SeriesResult {
                labels: LabelSet::clone(&series.labels),
                samples,
            });
        }
        Ok(out)
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

    fn labels(host: &str) -> LabelSet {
        LabelSet::from_pairs([("host", host)])
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
        assert!(
            db.index
                .series(&[])
                .iter()
                .all(|series| series.lock().samples().is_empty())
        );

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
    fn concurrent_writes_and_queries_end_in_complete_state() {
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
                        db.query(&[], TimeRange::new(0, u64::MAX)).unwrap();
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
