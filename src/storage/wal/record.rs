use crate::model::{LabelSet, Sample, SeriesId};
use strum_macros::FromRepr;

/// Type of record stored in the WAL.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromRepr)]
pub enum RecordKind {
    /// Defines a series and its labels.
    Series = 0,
    /// Stores samples belonging to an existing series.
    Samples = 1,
}

#[derive(Debug)]
pub struct SeriesRecord {
    pub id: SeriesId,
    pub labels: LabelSet,
}

#[derive(Debug)]
pub struct SamplesRecord {
    pub id: SeriesId,
    pub samples: Vec<Sample>,
}

/// Decoded logical WAL record.
#[derive(Debug)]
pub enum WalRecord {
    Series(SeriesRecord),
    Samples(SamplesRecord),
}
