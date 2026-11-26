use super::buffer::Taken;
use super::lsn::Lsn;
use super::segment::ActiveSegment;
use super::{Shared, WalConfig};
use std::io;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

/// The WAL threads: the flusher writes the buffer to the active segment,
/// rotates and serves cuts, the syncer fdatasyncs it every `sync_interval`.
pub(crate) struct Threads {
    flusher: Option<JoinHandle<()>>,
    syncer: Option<JoinHandle<()>>,
}

/// Hands the id of the segment closed at a cut back to `Wal::wait_cut`.
#[derive(Default)]
pub(crate) struct Cut {
    state: Mutex<Option<u64>>,
    done: Condvar,
}

impl Cut {
    fn finish(&self, closed: u64) {
        let mut st = self.state.lock().unwrap();
        *st = Some(closed);
        self.done.notify_all();
    }

    pub(crate) fn wait(&self) -> u64 {
        let st = self.state.lock().unwrap();
        let mut st = self.done.wait_while(st, |s| s.is_none()).unwrap();
        st.take().expect("set by finish")
    }
}

impl Threads {
    pub(crate) fn spawn(
        shared: &Arc<Shared>,
        segment: ActiveSegment,
        cfg: &WalConfig,
    ) -> io::Result<Threads> {
        let syncer = {
            let (shared, cfg) = (Arc::clone(shared), cfg.clone());
            thread::Builder::new()
                .name("wal-sync".into())
                .spawn(move || Self::sync_loop(&shared, &cfg))?
        };

        let flusher = {
            let (shared, cfg) = (Arc::clone(shared), cfg.clone());
            thread::Builder::new()
                .name("wal-flush".into())
                .spawn(move || Self::flush_loop(&shared, segment, &cfg))?
        };

        shared.progress.set_flusher(flusher.thread().clone());

        Ok(Threads {
            flusher: Some(flusher),
            syncer: Some(syncer),
        })
    }

    pub(crate) fn join(&mut self) -> io::Result<()> {
        for h in [self.flusher.take(), self.syncer.take()]
            .into_iter()
            .flatten()
        {
            h.thread().unpark();
            h.join()
                .map_err(|_| io::Error::other("wal thread panicked"))?;
        }

        Ok(())
    }

    fn flush_loop(shared: &Shared, mut seg: ActiveSegment, cfg: &WalConfig) {
        let progress = &shared.progress;
        let mut spare = Vec::with_capacity(cfg.buffer_capacity);

        loop {
            let closing = shared.buffer.is_closed();

            if !closing
                && !cfg.commit_delay.is_zero()
                && progress.has_waiters()
                && shared.buffer.len() < cfg.commit_bytes
            {
                thread::sleep(cfg.commit_delay);
            }

            match shared.buffer.take(&mut spare) {
                Some(Taken { end, cut }) => {
                    let n = spare.len();

                    if n > 0 {
                        write_batch(shared, &mut seg, &mut spare, end, cfg);
                    }
                    if cut {
                        let closed = rotate(&mut seg, shared, cfg);
                        shared.cut.finish(closed);
                        continue;
                    }
                    if closing || n >= cfg.flush_threshold || progress.has_waiters() {
                        continue;
                    }
                }
                None if closing => break,
                None => {}
            }

            thread::park_timeout(cfg.flush_interval);
        }

        seg.sync().unwrap_or_else(|e| fatal("final fdatasync", &e));
    }

    fn sync_loop(shared: &Shared, cfg: &WalConfig) {
        let mut last = Lsn(0);
        let mut deadline = Instant::now() + cfg.sync_interval;

        while !shared.buffer.is_closed() {
            thread::park_timeout(deadline.saturating_duration_since(Instant::now()));

            if Instant::now() < deadline {
                continue;
            }
            deadline = Instant::now() + cfg.sync_interval;

            let w = shared.progress.written();
            if w > last {
                let file = Arc::clone(&shared.sync_file.lock().unwrap());
                file.sync_data().unwrap_or_else(|e| fatal("fdatasync", &e));
                last = w;
            }
        }
    }
}

fn write_batch(
    shared: &Shared,
    seg: &mut ActiveSegment,
    spare: &mut Vec<u8>,
    end: Lsn,
    cfg: &WalConfig,
) {
    if seg.written() >= cfg.segment_max_bytes {
        rotate(seg, shared, cfg);
    }

    seg.write(spare, cfg.writeback)
        .unwrap_or_else(|e| fatal("write", &e));

    shared.progress.publish_written(end);

    spare.clear();
    if spare.capacity() > 4 * cfg.buffer_capacity {
        spare.shrink_to(cfg.buffer_capacity);
    }
}

fn rotate(seg: &mut ActiveSegment, shared: &Shared, cfg: &WalConfig) -> u64 {
    let closed = seg.info.id;

    seg.sync()
        .unwrap_or_else(|e| fatal("fdatasync on rotation", &e));

    *seg = ActiveSegment::create(&cfg.dir, closed + 1)
        .unwrap_or_else(|e| fatal("segment creation", &e));

    *shared.sync_file.lock().unwrap() = seg.file();
    shared.active_segment.store(closed + 1, Relaxed);

    closed
}

fn fatal(what: &str, e: &io::Error) -> ! {
    eprintln!("wal: {what} failed: {e}; aborting (recovery will replay the log)");
    std::process::abort();
}
