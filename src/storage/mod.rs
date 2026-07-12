//! Storage interfaces and in-memory implementations.

use crate::model::{LabelSet, Matcher, Sample, SeriesId, TimeRange};
use thiserror::Error;

pub mod index;
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

pub trait SampleStore {
    /// Appends a sample to the series identified by the given series ID.
    fn append(&mut self, id: SeriesId, sample: Sample) -> Result<(), StorageError>;

    /// Reads samples from a series within the specified time range.
    fn read(&self, id: SeriesId, range: TimeRange) -> Result<Vec<Sample>, StorageError>;
}

pub trait SeriesIndex {
    /// Encodes a label set, returning its existing or newly allocated series ID.
    fn encode(&mut self, labels: &LabelSet) -> SeriesId;

    /// Resolves matchers to their matching series IDs.
    fn resolve(&self, matchers: &[Matcher]) -> Vec<SeriesId>;

    /// Returns the label set for a series ID, if it exists.
    fn labels_for(&self, id: SeriesId) -> Option<LabelSet>;

    fn including_label(&self, label_name: &str) -> Vec<SeriesId>;
}
