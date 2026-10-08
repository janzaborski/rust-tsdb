//! Storage interfaces and in-memory implementations.

use thiserror::Error;

pub mod index;
pub mod mem_table;
pub mod segment;

pub use index::Index;
pub use mem_table::MemTable;

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
