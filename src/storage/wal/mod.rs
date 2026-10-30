mod buffer;
pub mod codec;
pub mod error;
mod flusher;
mod lsn;
pub mod reader;
pub mod record;
pub mod replay;
pub mod segment;
mod writer;

pub use codec::LabelSetEncoding;
pub use error::{CodecError, Corruption, WalError};
pub use lsn::{Durability, Lsn};
pub use reader::RecoveryReport;
pub use record::{RecordKind, SamplesEntry, SeriesEntry, WalRecord};
pub use replay::{ReplaySink, ResolveStats};
pub use writer::WalWriter;

use buffer::WalBuffer;
use flusher::{Cut, Threads};
use lsn::Progress;
use segment::ActiveSegment;
use std::fs::{self, File};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct WalConfig {
    pub dir: PathBuf,
    pub segment_max_bytes: u64,
    pub flush_interval: Duration,
    pub flush_threshold: usize,
    pub buffer_capacity: usize,
    pub sync_interval: Duration,
    pub writeback: bool,
    pub commit_delay: Duration,
    pub commit_bytes: usize,
}

impl WalConfig {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            segment_max_bytes: 128 << 20,
            flush_interval: Duration::from_millis(100),
            flush_threshold: 4 << 20,
            buffer_capacity: 8 << 20,
            sync_interval: Duration::from_secs(1),
            writeback: true,
            commit_delay: Duration::from_micros(200),
            commit_bytes: 256 << 10,
        }
    }
}

pub(crate) struct Shared {
    pub(crate) buffer: WalBuffer,
    pub(crate) progress: Progress,
    pub(crate) sync_file: Mutex<Arc<File>>,
    pub(crate) active_segment: AtomicU64,
    pub(crate) cut: Cut,
}

pub struct Wal {
    shared: Arc<Shared>,
    config_dir: PathBuf,
    threads: Threads,
    recovery: RecoveryReport,
}

impl Wal {
    pub fn open(config: WalConfig, sink: &mut impl ReplaySink) -> Result<Self, WalError> {
        let dir_existed = config.dir.exists();
        fs::create_dir_all(&config.dir)?;

        if !dir_existed
            && let Some(parent) = config.dir.parent().filter(|p| !p.as_os_str().is_empty())
        {
            segment::sync_dir(parent)?;
        }

        let recovery = reader::replay(&config.dir, sink)?;
        let segment =
            ActiveSegment::create(&config.dir, recovery.last_segment.map_or(1, |id| id + 1))?;

        let shared = Arc::new(Shared {
            buffer: WalBuffer::new(config.buffer_capacity, config.flush_threshold),
            progress: Progress::default(),
            sync_file: Mutex::new(segment.file()),
            active_segment: AtomicU64::new(segment.info.id),
            cut: Cut::default(),
        });

        let threads = Threads::spawn(&shared, segment, &config)?;

        Ok(Self {
            shared,
            config_dir: config.dir,
            threads,
            recovery,
        })
    }

    pub fn writer(&self) -> WalWriter {
        WalWriter::new(Arc::clone(&self.shared))
    }

    pub fn cut(&self) -> u64 {
        self.shared
            .cut
            .request(|| self.shared.progress.unpark_flusher())
    }

    pub fn remove_through(&self, id: u64) -> Result<usize, WalError> {
        let id = id.min(self.shared.active_segment.load(Relaxed).saturating_sub(1));
        let mut removed = 0;

        for seg in segment::list(&self.config_dir)?
            .into_iter()
            .filter(|s| s.id <= id)
        {
            fs::remove_file(&seg.path)?;
            removed += 1;
        }

        if removed > 0 {
            segment::sync_dir(&self.config_dir)?;
        }

        Ok(removed)
    }

    pub fn recovery(&self) -> &RecoveryReport {
        &self.recovery
    }

    pub fn close(mut self) -> Result<(), WalError> {
        self.shared.buffer.close();
        Ok(self.threads.join()?)
    }
}

impl Drop for Wal {
    fn drop(&mut self) {
        self.shared.buffer.close();
        let _ = self.threads.join();
    }
}
