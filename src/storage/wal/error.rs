use thiserror::Error;

#[derive(Error, Debug)]
pub enum Corruption {
    #[error("record cut short")]
    Truncated,

    #[error("invalid record kind: {0}")]
    InvalidKind(u8),

    #[error("record length exceeds maximum: {0}")]
    InvalidLength(u32),

    #[error("checksum mismatch")]
    ChecksumMismatch,

    #[error("invalid record payload: {0}")]
    InvalidPayload(#[from] CodecError),
}

#[derive(Error, Debug)]
pub enum CodecError {
    #[error("unexpected end of input")]
    UnexpectedEof,

    #[error("invalid utf-8 in label")]
    BadUtf8,

    #[error("trailing bytes after record")]
    TrailingBytes,
}

#[derive(Error, Debug)]
pub enum WalError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("record too large: {0} bytes")]
    RecordTooLarge(usize),

    #[error("corrupt record in segment {segment_id} at offset {offset}: {reason}")]
    Corrupt {
        segment_id: u64,
        offset: u64,
        reason: Corruption,
    },

    #[error("WAL is closed")]
    Closed,
}
