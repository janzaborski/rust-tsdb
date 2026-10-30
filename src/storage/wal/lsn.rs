use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::thread::Thread;
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Lsn(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    None,
    Written,
}

/// The published `written` LSN, a count for group
/// commit and the flusher's handle to wake it.
#[derive(Default)]
pub struct Progress {
    written: watch::Sender<Lsn>,
    waiters: AtomicUsize,
    flusher: OnceLock<Thread>,
}

fn advance(cur: &mut Lsn, to: Lsn) -> bool {
    if to > *cur {
        *cur = to;
        true
    } else {
        false
    }
}

struct WaiterGuard<'a>(&'a AtomicUsize);

impl<'a> WaiterGuard<'a> {
    fn new(c: &'a AtomicUsize) -> Self {
        c.fetch_add(1, Relaxed);
        Self(c)
    }
}

impl Drop for WaiterGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Relaxed);
    }
}

impl Progress {
    pub fn set_flusher(&self, flusher: Thread) {
        let _ = self.flusher.set(flusher);
    }

    pub fn written(&self) -> Lsn {
        *self.written.borrow()
    }

    pub fn has_waiters(&self) -> bool {
        self.waiters.load(Relaxed) > 0
    }

    pub fn unpark_flusher(&self) {
        if let Some(t) = self.flusher.get() {
            t.unpark();
        }
    }

    pub fn publish_written(&self, lsn: Lsn) {
        self.written.send_if_modified(|w| advance(w, lsn));
    }

    pub async fn wait(&self, lsn: Lsn, level: Durability) {
        if level == Durability::None || self.written() >= lsn {
            return;
        }
        let _guard = WaiterGuard::new(&self.waiters);
        let mut rx = self.written.subscribe();
        self.unpark_flusher();
        let _ = rx.wait_for(|&curr| curr >= lsn).await;
    }
}
