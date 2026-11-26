use super::error::WalError;
use super::lsn::Lsn;
use std::mem;
use std::sync::Mutex;

/// Records waiting to be written. Writers encode outside the lock and only
/// copy in under it, the flusher swaps the whole Vec out.
pub(crate) struct WalBuffer {
    inner: Mutex<Inner>,
    flush_threshold: usize,
}

struct Inner {
    buf: Vec<u8>,
    end: u64,
    closed: bool,
    cut_at: Option<u64>,
}

pub(crate) struct Appended {
    pub lsn: Lsn,
    pub crossed_threshold: bool,
}

pub(crate) struct Taken {
    pub end: Lsn,
    pub cut: bool,
}

impl WalBuffer {
    pub fn new(capacity: usize, flush_threshold: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                buf: Vec::with_capacity(capacity),
                end: 0,
                closed: false,
                cut_at: None,
            }),
            flush_threshold,
        }
    }

    pub fn append(&self, record: &[u8]) -> Result<Appended, WalError> {
        let mut g = self.inner.lock().unwrap();

        if g.closed {
            return Err(WalError::Closed);
        }

        let before = g.buf.len();
        g.buf.extend_from_slice(record);
        g.end += record.len() as u64;

        Ok(Appended {
            lsn: Lsn(g.end),
            crossed_threshold: before < self.flush_threshold && g.buf.len() >= self.flush_threshold,
        })
    }

    pub fn take(&self, spare: &mut Vec<u8>) -> Option<Taken> {
        let mut g = self.inner.lock().unwrap();

        let Some(cut_at) = g.cut_at.take() else {
            if g.buf.is_empty() {
                return None;
            }
            mem::swap(&mut g.buf, spare);
            return Some(Taken {
                end: Lsn(g.end),
                cut: false,
            });
        };

        let len = g.buf.len();
        let offset = (cut_at - (g.end - len as u64)) as usize;

        if offset == len {
            mem::swap(&mut g.buf, spare);
        } else {
            spare.extend_from_slice(&g.buf[..offset]);
            g.buf.drain(..offset);
        }

        Some(Taken {
            end: Lsn(cut_at),
            cut: true,
        })
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().buf.len()
    }

    pub fn end(&self) -> Lsn {
        Lsn(self.inner.lock().unwrap().end)
    }

    pub fn close(&self) {
        self.inner.lock().unwrap().closed = true;
    }

    pub fn is_closed(&self) -> bool {
        self.inner.lock().unwrap().closed
    }

    pub fn mark_cut(&self) -> Result<Lsn, WalError> {
        let mut g = self.inner.lock().unwrap();

        if g.closed {
            return Err(WalError::Closed);
        }

        g.cut_at = Some(g.end);
        Ok(Lsn(g.end))
    }
}
