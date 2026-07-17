use super::WalConfig;
use super::error::WalError;
use super::record::RecordKind;
use super::segment::Segment;

use super::MAX_RECORD_BYTES;

/// Low-level WAL writer.
pub struct WalWriter {
    config: WalConfig,
    active: Segment,
}

impl WalWriter {
    pub fn create(config: WalConfig, segment_id: u64) -> Result<Self, WalError> {
        let active = Segment::create(&config.dir, segment_id)?;

        Ok(Self { config, active })
    }

    pub fn append(&mut self, kind: RecordKind, payload: &[u8]) -> Result<(), WalError> {
        if payload.len() > MAX_RECORD_BYTES as usize {
            return Err(WalError::RecordTooLarge(payload.len()));
        }

        if self.active.written >= self.config.segment_max_bytes {
            self.rotate()?;
        }

        self.active.append_record(kind, payload)?;
        self.active.flush_and_sync()
    }

    pub fn sync(&mut self) -> Result<(), WalError> {
        self.active.flush_and_sync()
    }

    fn rotate(&mut self) -> Result<(), WalError> {
        self.active.flush_and_sync()?;

        let next_id = self.active.info.id + 1;
        let new_segment = Segment::create(&self.config.dir, next_id)?;

        self.active = new_segment;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn create_starts_with_requested_segment() {
        let dir = tempdir().unwrap();

        let config = WalConfig {
            dir: dir.path().to_path_buf(),
            segment_max_bytes: 1024,
        };

        let writer = WalWriter::create(config, 5).unwrap();

        assert_eq!(writer.active.info.id, 5);
    }
}
