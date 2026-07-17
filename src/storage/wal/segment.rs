use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use super::codec::RecordHeader;
use super::error::WalError;
use super::record::RecordKind;

/// Identifies a WAL segment on disk.
#[derive(Debug, Clone)]
pub struct SegmentInfo {
    pub id: u64,
    pub path: PathBuf,
}

/// Writable WAL segment.
pub struct Segment {
    pub info: SegmentInfo,
    pub file: BufWriter<File>,
    pub written: u64,
}

impl Segment {
    pub fn file_name(id: u64) -> String {
        format!("wal-{:08}.log", id)
    }

    pub fn parse_id(path: &Path) -> Option<u64> {
        let stem = path.file_name()?.to_str()?;
        let num = stem.strip_prefix("wal-")?.strip_suffix(".log")?;
        num.parse().ok()
    }

    pub fn create(dir: &Path, id: u64) -> Result<Self, WalError> {
        let new_file_path = dir.join(Self::file_name(id));

        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&new_file_path)?;

        Self::sync_dir(dir)?;

        Ok(Self {
            info: SegmentInfo {
                id,
                path: new_file_path,
            },
            file: BufWriter::new(file),
            written: 0,
        })
    }

    pub fn append_record(&mut self, kind: RecordKind, payload: &[u8]) -> Result<(), WalError> {
        let header = RecordHeader::for_record(kind, payload);
        header.write_to(&mut self.file)?;
        self.file.write_all(payload)?;
        self.written += (RecordHeader::ENCODED_LEN + payload.len()) as u64;

        Ok(())
    }

    pub fn flush_and_sync(&mut self) -> Result<(), WalError> {
        self.file.flush()?;
        self.file.get_ref().sync_data()?;

        Ok(())
    }

    fn sync_dir(dir: &Path) -> Result<(), WalError> {
        File::open(dir)?.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn file_name_contains_padded_segment_id() {
        assert_eq!(Segment::file_name(1), "wal-00000001.log");
        assert_eq!(Segment::file_name(42), "wal-00000042.log");
    }

    #[test]
    fn parse_id_reads_valid_segment_name() {
        let path = Path::new("wal-00000042.log");

        assert_eq!(Segment::parse_id(path), Some(42));
    }

    #[test]
    fn parse_id_rejects_unrelated_file() {
        assert_eq!(Segment::parse_id(Path::new("something.txt")), None);
    }

    #[test]
    fn create_creates_segment_file() {
        let dir = tempdir().unwrap();

        let segment = Segment::create(dir.path(), 1).unwrap();

        assert_eq!(segment.info.id, 1);
        assert!(segment.info.path.exists());
        assert_eq!(segment.written, 0);
    }
}
