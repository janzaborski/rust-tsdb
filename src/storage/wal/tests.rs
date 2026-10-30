use super::*;
use crate::model::{Label, LabelSet, Sample, SeriesId};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};

fn labels(v: &str) -> LabelSet {
    LabelSet::from_labels([Label::new("s", v)])
}

fn s(t: u64) -> Sample {
    Sample::new(t, t as f64)
}

#[derive(Default)]
struct Sink {
    ids: HashMap<LabelSet, SeriesId>,
    samples: HashMap<SeriesId, Vec<Sample>>,
}

impl Sink {
    fn of(&self, l: &LabelSet) -> Vec<Sample> {
        self.ids
            .get(l)
            .and_then(|id| self.samples.get(id))
            .cloned()
            .unwrap_or_default()
    }
}

impl ReplaySink for Sink {
    fn series(&mut self, labels: LabelSet) -> SeriesId {
        let next = SeriesId(1000 + self.ids.len() as u64);
        *self.ids.entry(labels).or_insert(next)
    }
    fn samples(&mut self, id: SeriesId, samples: Vec<Sample>) {
        self.samples.entry(id).or_default().extend(samples);
    }
}

fn open(dir: &std::path::Path) -> (Wal, Sink) {
    let mut sink = Sink::default();
    let wal = Wal::open(WalConfig::new(dir), &mut sink).unwrap();
    (wal, sink)
}

fn reopen(dir: &std::path::Path) -> Sink {
    let (wal, sink) = open(dir);
    wal.close().unwrap();
    sink
}

fn last_segment(dir: &std::path::Path) -> std::path::PathBuf {
    segment::list(dir).unwrap().pop().unwrap().path
}

#[test]
fn replay_returns_what_was_appended() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (labels("a"), labels("b"));
    let stale = Sample::new(3, f64::NAN);
    {
        let (wal, _) = open(dir.path());
        let w = wal.writer();
        w.append([(SeriesId(1), &a)], [(SeriesId(1), &[s(1)][..])])
            .unwrap();
        w.append(
            [(SeriesId(2), &b)],
            [
                (SeriesId(1), &[s(2), stale][..]),
                (SeriesId(2), &[s(1)][..]),
            ],
        )
        .unwrap();
        wal.close().unwrap();
    }
    let sink = reopen(dir.path());
    let got = sink.of(&a);
    assert_eq!(got[..2], [s(1), s(2)]);
    assert!(got[2].value.is_nan(), "NaN (stale marker) survives");
    assert_eq!(sink.of(&b), [s(1)]);
}

#[test]
fn torn_tail_is_repaired_and_earlier_records_kept() {
    let dir = tempfile::tempdir().unwrap();
    {
        let (wal, _) = open(dir.path());
        let w = wal.writer();
        w.append([(SeriesId(1), &labels("a"))], [(SeriesId(1), &[s(1)][..])])
            .unwrap();
        w.append([(SeriesId(2), &labels("b"))], [(SeriesId(2), &[s(1)][..])])
            .unwrap();
        wal.close().unwrap();
    }
    let path = last_segment(dir.path());
    let len = std::fs::metadata(&path).unwrap().len();
    OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(len - 3)
        .unwrap();

    let sink = reopen(dir.path());
    assert_eq!(sink.of(&labels("a")), [s(1)]);
    assert!(sink.of(&labels("b")).is_empty());
    assert_eq!(
        reopen(dir.path()).of(&labels("a")),
        [s(1)],
        "repair is persistent"
    );
}

#[test]
fn corruption_before_the_last_segment_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    for _ in 0..2 {
        let (wal, _) = open(dir.path());
        wal.writer()
            .append([(SeriesId(1), &labels("a"))], [(SeriesId(1), &[s(1)][..])])
            .unwrap();
        wal.close().unwrap();
    }
    let first = segment::list(dir.path()).unwrap().remove(0).path;
    let mut f = OpenOptions::new().write(true).open(first).unwrap();
    f.seek(SeekFrom::Start(20)).unwrap();
    f.write_all(&[0xFF]).unwrap();

    let err = Wal::open(WalConfig::new(dir.path()), &mut Sink::default()).err();
    assert!(matches!(err, Some(WalError::Corrupt { .. })));
}

#[test]
fn samples_logged_before_their_series_are_delivered() {
    let dir = tempfile::tempdir().unwrap();
    {
        let (wal, _) = open(dir.path());
        let w = wal.writer();
        w.append_samples([(SeriesId(42), &[s(2)][..])]).unwrap();
        w.append(
            [(SeriesId(42), &labels("a"))],
            [(SeriesId(42), &[s(1)][..])],
        )
        .unwrap();
        w.append_samples([(SeriesId(9), &[s(1)][..])]).unwrap(); // never defined
        wal.close().unwrap();
    }
    let (wal, sink) = open(dir.path());
    let mut got = sink.of(&labels("a"));
    got.sort_by_key(|x| x.timestamp);
    assert_eq!(got, [s(1), s(2)]);
    assert_eq!(wal.recovery().series.dropped_samples, 1);
}

#[test]
fn wal_ids_are_mapped_through_series_records() {
    let dir = tempfile::tempdir().unwrap();
    {
        let (wal, _) = open(dir.path());
        let w = wal.writer();
        w.append([(SeriesId(7), &labels("a"))], [(SeriesId(7), &[s(1)][..])])
            .unwrap();
        w.append(
            [(SeriesId(1000), &labels("b"))],
            [(SeriesId(1000), &[s(5)][..])],
        )
        .unwrap();
        wal.close().unwrap();
    }
    let sink = reopen(dir.path());
    assert_eq!(sink.of(&labels("a")), [s(1)]);
    assert_eq!(sink.of(&labels("b")), [s(5)]);
}

#[test]
fn cut_and_remove_drop_only_what_came_before() {
    let dir = tempfile::tempdir().unwrap();
    {
        let (wal, _) = open(dir.path());
        let w = wal.writer();
        w.append([(SeriesId(1), &labels("a"))], [(SeriesId(1), &[s(1)][..])])
            .unwrap();
        let cut = wal.cut();
        w.append([(SeriesId(1), &labels("a"))], [(SeriesId(1), &[s(2)][..])])
            .unwrap();
        wal.remove_through(cut).unwrap();
        wal.remove_through(u64::MAX).unwrap(); // never touches the active segment
        wal.close().unwrap();
    }
    assert_eq!(reopen(dir.path()).of(&labels("a")), [s(2)]);
}

#[test]
fn close_writes_everything_and_rejects_later_appends() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = WalConfig::new(dir.path());
    cfg.flush_interval = std::time::Duration::from_secs(3600); // only close can flush
    let wal = Wal::open(cfg, &mut Sink::default()).unwrap();
    let w = wal.writer();
    w.append([(SeriesId(1), &labels("a"))], [(SeriesId(1), &[s(1)][..])])
        .unwrap();
    wal.close().unwrap();

    assert!(matches!(
        w.append_samples([(SeriesId(1), &[s(2)][..])]),
        Err(WalError::Closed)
    ));
    assert_eq!(reopen(dir.path()).of(&labels("a")), [s(1)]);
}

#[test]
fn oversized_record_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let (wal, _) = open(dir.path());
    let big = vec![s(0); record::MAX_RECORD_BYTES as usize / 16 + 1];
    let err = wal
        .writer()
        .append_samples([(SeriesId(1), &big[..])])
        .unwrap_err();
    assert!(matches!(err, WalError::RecordTooLarge(_)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn written_wait_does_not_depend_on_the_flush_timer() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = WalConfig::new(dir.path());
    cfg.flush_interval = std::time::Duration::from_secs(3600);
    let wal = Wal::open(cfg, &mut Sink::default()).unwrap();
    let w = wal.writer();

    let lsn = w.append_samples([(SeriesId(1), &[s(1)][..])]).unwrap();
    let waited = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        w.wait(lsn, Durability::Written),
    );
    assert!(waited.await.is_ok(), "a waiter must wake the flusher");
    assert!(w.written_lsn() >= lsn);

    tokio::task::spawn_blocking(move || wal.close())
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn concurrent_appends_replay_completely() {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = WalConfig::new(dir.path());
    cfg.segment_max_bytes = 64 << 10;
    cfg.flush_threshold = 8 << 10;
    let wal = Wal::open(cfg, &mut Sink::default()).unwrap();
    let next = AtomicU64::new(1);
    let published: Vec<AtomicU64> = (0..32).map(|_| AtomicU64::new(0)).collect();
    let logged = AtomicU64::new(0);

    std::thread::scope(|scope| {
        for t in 0..8u64 {
            let (w, next, published, logged) = (wal.writer(), &next, &published, &logged);
            scope.spawn(move || {
                for i in 0..1000u64 {
                    let mut created = Vec::new();
                    let mut batch = Vec::new();
                    if i % 4 == 0 {
                        let id = next.fetch_add(1, Relaxed);
                        published[(id % 32) as usize].store(id, Relaxed); // visible before logged
                        created.push((SeriesId(id), labels(&id.to_string())));
                        batch.push((SeriesId(id), [Sample::new(i, id as f64)]));
                    }
                    let id = published[((t * 31 + i) % 32) as usize].load(Relaxed);
                    if id != 0 {
                        batch.push((SeriesId(id), [Sample::new(i, id as f64)]));
                    }
                    logged.fetch_add(batch.len() as u64, Relaxed);
                    w.append(
                        created.iter().map(|(id, l)| (*id, l)),
                        batch.iter().map(|(id, s)| (*id, &s[..])),
                    )
                    .unwrap();
                }
            });
        }
    });
    wal.close().unwrap();

    let (wal, sink) = open(dir.path());
    assert_eq!(wal.recovery().series.dropped_samples, 0);
    assert_eq!(
        sink.samples.values().map(Vec::len).sum::<usize>() as u64,
        logged.load(Relaxed)
    );
    for (l, id) in &sink.ids {
        let wal_id: f64 = l.get("s").unwrap().parse().unwrap();
        assert!(
            sink.samples[id].iter().all(|x| x.value == wal_id),
            "samples on the wrong series"
        );
    }
}
