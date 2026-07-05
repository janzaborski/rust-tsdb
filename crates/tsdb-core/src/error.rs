use thiserror::Error;

#[derive(Error, Debug)]
pub enum DbError {
    #[error(transparent)]
    Storage(#[from] StorageError),

    #[error("Invalid write batch: {0}")]
    InvalidWriteBatch(String),
}

#[derive(Error, Debug)]
pub enum StorageError {
    #[error("Failed to append sample: {0}")]
    AppendSample(String),

    #[error("Failed to read samples: {0}")]
    ReadSamples(String),
}
