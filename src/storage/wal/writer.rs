use super::Shared;
use super::codec::{LabelSetEncoding, RecordEncoder};
use super::error::WalError;
use super::lsn::{Durability, Lsn};
use super::record::MAX_RECORD_BYTES;
use crate::model::{LabelSet, Sample, SeriesId};
use std::cell::RefCell;
use std::sync::Arc;

const SCRATCH_KEEP: usize = 1 << 20;

thread_local! {
    /// Per-thread encoding buffer: a record is built here without any lock,
    /// then copied into `WalBuffer`.
    static SCRATCH: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Handle for appending to the WAL and waiting on durability.
#[derive(Clone)]
pub struct WalWriter {
    shared: Arc<Shared>,
}

impl WalWriter {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    pub fn append<'a, 'b, L, S, M>(&self, series: S, samples: M) -> Result<Lsn, WalError>
    where
        L: LabelSetEncoding + 'a,
        S: IntoIterator<Item = (SeriesId, &'a L)>,
        M: IntoIterator<Item = (SeriesId, &'b [Sample])>,
    {
        self.append_with(|enc| {
            for (id, labels) in series {
                enc.series(id, labels);
            }
            for (id, s) in samples {
                enc.samples(id, s);
            }
        })
    }

    pub fn append_samples<'b, M>(&self, samples: M) -> Result<Lsn, WalError>
    where
        M: IntoIterator<Item = (SeriesId, &'b [Sample])>,
    {
        self.append(std::iter::empty::<(SeriesId, &LabelSet)>(), samples)
    }

    pub async fn wait(&self, lsn: Lsn, level: Durability) {
        self.shared.progress.wait(lsn, level).await
    }

    pub fn written_lsn(&self) -> Lsn {
        self.shared.progress.written()
    }

    pub fn backlog_bytes(&self) -> u64 {
        self.shared.buffer.end().0 - self.shared.progress.written().0
    }

    fn append_with(&self, fill: impl FnOnce(&mut RecordEncoder<'_>)) -> Result<Lsn, WalError> {
        SCRATCH.with_borrow_mut(|buf| {
            let mut enc = RecordEncoder::new(buf);
            fill(&mut enc);
            let (empty, payload_len) = enc.finish();

            let result = if empty {
                Ok(self.shared.buffer.end()) // nothing to log
            } else if payload_len > MAX_RECORD_BYTES as usize {
                Err(WalError::RecordTooLarge(payload_len))
            } else {
                self.shared.buffer.append(buf).map(|a| {
                    if a.crossed_threshold {
                        self.shared.progress.unpark_flusher();
                    }
                    a.lsn
                })
            };

            if buf.capacity() > SCRATCH_KEEP {
                buf.clear();
                buf.shrink_to(SCRATCH_KEEP);
            }
            result
        })
    }
}
