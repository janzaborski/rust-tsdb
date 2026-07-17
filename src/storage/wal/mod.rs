use crate::model::{LabelSet, Sample, SeriesId};
use std::fs::{File, create_dir_all};
use std::path::{Path, PathBuf};

mod codec;
mod reader;
mod record;
mod segment;
mod writer;

mod error;

use codec::Codec;
pub use error::WalError;
use reader::WalReader;
pub use record::{RecordKind, WalRecord};
use writer::WalWriter;

const MAX_RECORD_BYTES: u32 = 64 * 1024 * 1024;
const SAMPLE_ENCODED_LEN: usize = size_of::<u64>() + size_of::<f64>();

/// Configuration for the write-ahead log.
pub struct WalConfig {
    /// Directory containing WAL segment files.
    pub dir: PathBuf,
    /// Size threshold after which the active segment is rotated.
    pub segment_max_bytes: u64,
}

/// Persistent write-ahead log.
pub struct Wal {
    codec: Codec,
    writer: WalWriter,
}

impl Wal {
    /// Opens an existing WAL or creates a new one.
    ///
    /// Existing segments are recovered first. A truncated record at the end
    /// of the final segment is discarded during recovery.
    pub fn open_or_create(config: WalConfig) -> Result<(Self, Vec<WalRecord>), WalError> {
        let dir_existed = config.dir.exists();

        create_dir_all(&config.dir)?;

        if !dir_existed {
            Self::sync_parent_dir(&config.dir)?;
        }

        let mut reader = WalReader::open(&config.dir)?;
        let next_id = reader.next_segment_id();
        let records = reader.recover()?;

        let writer = WalWriter::create(config, next_id)?;

        Ok((
            Self {
                codec: Codec::new(),
                writer,
            },
            records,
        ))
    }

    pub fn append_series(&mut self, id: SeriesId, labels: &LabelSet) -> Result<(), WalError> {
        let payload = self.codec.encode_series(id, labels);
        self.writer.append(RecordKind::Series, payload)
    }

    pub fn append_samples(&mut self, id: SeriesId, samples: &[Sample]) -> Result<(), WalError> {
        let payload = self.codec.encode_samples(id, samples);
        self.writer.append(RecordKind::Samples, payload)
    }

    pub fn sync(&mut self) -> Result<(), WalError> {
        self.writer.sync()
    }

    fn sync_parent_dir(path: &Path) -> Result<(), WalError> {
        if let Some(parent) = path.parent() {
            File::open(parent)?.sync_all()?;
        }

        Ok(())
    }
}
