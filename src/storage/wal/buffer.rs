use super::error::WalError;
use super::lsn::Lsn;
use std::mem;
use std::sync::Mutex;

pub(crate) struct WalBuffer {
    inner: Mutex<Inner>,
    flush_threshold: usize,
}

struct Inner {
    buf: Vec<u8>,
    end: u64,
    closed: bool,
}

pub(crate) struct Appended {
    pub lsn: Lsn,
    pub crossed_threshold: bool,
}

impl WalBuffer {
    pub fn new(capacity: usize, flush_threshold: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                buf: Vec::with_capacity(capacity),
                end: 0,
                closed: false,
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

    pub fn take(&self, spare: &mut Vec<u8>) -> Option<Lsn> {
        let mut g = self.inner.lock().unwrap();

        if g.buf.is_empty() {
            return None;
        }

        mem::swap(&mut g.buf, spare);

        Some(Lsn(g.end))
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
}
