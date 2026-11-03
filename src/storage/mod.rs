//! Storage interfaces and in-memory implementations.

use thiserror::Error;

pub mod index;
pub mod segment;
pub mod series;
pub mod wal;

pub use index::Index;
pub use series::Series;

#[derive(Error, Debug)]
pub enum StorageError {
    #[error("Failed to append sample: {0}")]
    AppendSample(String),

    #[error("Failed to read samples: {0}")]
    ReadSamples(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("Corrupt segment: {0}")]
    CorruptSegment(String),

    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
}
