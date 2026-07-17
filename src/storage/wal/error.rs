use thiserror::Error;

use super::codec::CodecError;

#[derive(Error, Debug)]
pub enum Corruption {
    #[error("Invalid record kind: {0}")]
    InvalidKind(u8),

    #[error("Record length exceeds maximum: {0}")]
    InvalidLength(u32),

    #[error("Checksum mismatch")]
    ChecksumMismatch,

    #[error("Invalid record payload: {0}")]
    InvalidPayload(CodecError),
}

#[derive(Error, Debug)]
pub enum WalError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("Record too large: {0}")]
    RecordTooLarge(usize),

    #[error("Truncated record in segment {segment_id} at offset {offset}")]
    TruncatedRecord { segment_id: u64, offset: u64 },

    #[error("corrupt record in segment {segment_id} at offset {offset}: {reason}")]
    CorruptRecord {
        segment_id: u64,
        offset: u64,
        reason: Corruption,
    },
}
