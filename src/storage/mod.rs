//! Storage interfaces and in-memory implementations.

use thiserror::Error;

pub mod index;
pub mod indexes;
pub mod mem_table;

pub use index::Index;
pub use mem_table::MemTable;

#[derive(Error, Debug)]
pub enum StorageError {
    #[error("Failed to append sample: {0}")]
    AppendSample(String),

    #[error("Failed to read samples: {0}")]
    ReadSamples(String),
}
