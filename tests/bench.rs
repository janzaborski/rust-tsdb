// tests/bench.rs — index microbenchmarks (store/WAL stubbed; measures the index only).
//
// Run:  cargo test --release --test bench -- --ignored --nocapture --test-threads=1
// Dev-dependency:  rand = { version = "0.9", features = ["small_rng"] }
//
// Row labels (every throughput/latency bench uses these four DBs):
//   single_lock  — SimpleIndex under one RwLock, encode per series
//   batch_single — SimpleIndex under one RwLock, one lock section per batch
//   concurrent   — ConcurrentIndex, per-series lock-free encode
//   batched      — ConcurrentIndex, batched lock-free encode
//
// Writer roles (shown on each config's header line):
//   hot   — re-append series drawn from the seeded hot set
//   mixed — hot series + an owned churn window that leaks `leak` new series/op
//   cold  — brand-new series every op (unbounded)

use tsdb::dbs::batched_index_db::BatchedIndexDb;
use tsdb::dbs::batched_single_lock_db::BatchedSingleLockDb;
use tsdb::dbs::concurrent_index_db::ConcurrentIndexDb;
use tsdb::dbs::single_lock_index_db::SingleLockIndexDb;
use tsdb::model::{
    Label, LabelSet, Matcher,
    MatcherOperator::{Equal, NotEqual},
    Sample, TimeRange,
};
use tsdb::{DbError, SeriesResult, WriteBatch};

use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;

// one interface over the DBs
trait Target: Send + Sync {
    fn seed(&self, b: WriteBatch) -> Result<(), DbError>;
    fn write(&self, b: WriteBatch) -> Result<(), DbError>;
    fn query(&self, m: &[Matcher], r: TimeRange) -> Result<Vec<SeriesResult>, DbError>;
}
macro_rules! impl_target {
    ($t:ty) => {
        impl Target for $t {
            fn seed(&self, b: WriteBatch) -> Result<(), DbError> {
                <$t>::seed(self, b)
            }
            fn write(&self, b: WriteBatch) -> Result<(), DbError> {
                <$t>::write(self, b)
            }
            fn query(&self, m: &[Matcher], r: TimeRange) -> Result<Vec<SeriesResult>, DbError> {
                <$t>::query(self, m, r)
            }
        }
    };
}
impl_target!(SingleLockIndexDb);
impl_target!(BatchedSingleLockDb);
impl_target!(ConcurrentIndexDb);
impl_target!(BatchedIndexDb);

fn rng(seed: u64) -> SmallRng {
    SmallRng::seed_from_u64(seed)
}

// workload label vocabulary
const SHARDS: u64 = 128;
const METRICS: &[&str] = &[
    "http_requests_total",
    "cpu_seconds_total",
    "mem_bytes",
    "disk_io",
    "net_bytes",
];
const REGIONS: &[&str] = &[
    "us-east-1",
    "us-west-2",
    "eu-west-1",
    "eu-central-1",
    "ap-south-1",
    "ap-ne-1",
];
const ENVS: &[&str] = &["prod", "staging", "dev"];
const SERVICES: u64 = 40;
const AZS: u64 = 24;
const VERSIONS: u64 = 8;

/// Seeded (~9-label) series, deterministic in `i`.
fn seeded_series_labels(i: u64) -> LabelSet {
    LabelSet::from_labels([
        Label::new("__name__", METRICS[(i % METRICS.len() as u64) as usize]),
        Label::new("region", REGIONS[(i % REGIONS.len() as u64) as usize]),
        Label::new("env", ENVS[((i / 6) % ENVS.len() as u64) as usize]),
        Label::new("az", format!("az-{}", i % AZS)),
        Label::new("service", format!("svc-{}", (i / 3) % SERVICES)),
        Label::new("version", format!("v{}", (i / 7) % VERSIONS)),
        Label::new("job", format!("job-{}", i % 10)),
        Label::new("shard", (i % SHARDS).to_string()),
        Label::new("instance", format!("inst-{i}")),
    ])
}

/// Pure-cold role: brand-new series, unbounded.
fn cold_series_labels(writer: usize, n: u64) -> LabelSet {
    LabelSet::from_labels([
        Label::new("__name__", "load"),
        Label::new("region", REGIONS[(n % REGIONS.len() as u64) as usize]),
        Label::new("env", ENVS[(n % ENVS.len() as u64) as usize]),
        Label::new("shard", (n % SHARDS).to_string()),
        Label::new("service", format!("svc-{}", n % SERVICES)),
        Label::new("writer", writer.to_string()),
        Label::new("series", n.to_string()),
    ])
}

/// Owned churn series; integer id, unique across writers via (w, id).
fn churn_labels(writer: usize, id: u64) -> LabelSet {
    LabelSet::from_labels([
        Label::new("__name__", "churn"),
        Label::new("region", REGIONS[(id % REGIONS.len() as u64) as usize]),
        Label::new("shard", (id % SHARDS).to_string()),
        Label::new("w", writer.to_string()),
        Label::new("id", id.to_string()),
    ])
}

#[derive(Clone, Copy)]
enum Shape {
    Point,
    Conj,
    Neg,
}

fn build_query(shape: Shape, r: &mut SmallRng) -> Vec<Matcher> {
    let shard = r.random_range(0..SHARDS).to_string();
    let region = REGIONS[r.random_range(0..REGIONS.len())];
    match shape {
        Shape::Point => vec![Matcher::new("shard", shard, Equal)],
        Shape::Conj => vec![
            Matcher::new("shard", shard, Equal),
            Matcher::new("region", region, Equal),
        ],
        Shape::Neg => vec![
            Matcher::new("shard", shard, Equal),
            Matcher::new("region", region, Equal),
            Matcher::new("env", "dev", NotEqual),
        ],
    }
}

// configs
#[derive(Clone, Copy)]
struct Config {
    name: &'static str,
    seed_series: u64,
    hot_set: u64,
    hot_writers: usize,
    hot_ops: u64,
    mixed_writers: usize,
    mixed_ops: u64,
    mixed_churn: usize, // active churn window per mixed writer
    leak: usize,        // new series/op per mixed writer (0 = fixed window)
    cold_writers: usize,
    cold_ops: u64,
    readers: usize,
    read_ops: u64,
    series_per_write: usize,
    shape: Shape,
    duration: Duration,
}

const LEAK_SWEEP: &[usize] = &[0, 5, 20, 50, 100, 200];

fn leak_config(base: Config, leak: usize) -> Config {
    Config { leak, ..base }
}

const SEEDED_MIXED: Config = Config {
    name: "seeded_mixed",
    seed_series: 50_000,
    hot_set: 10_000,
    hot_writers: 4,
    hot_ops: 20_000,
    mixed_writers: 0,
    mixed_ops: 0,
    mixed_churn: 0,
    leak: 0,
    cold_writers: 2,
    cold_ops: 3_000,
    readers: 8,
    read_ops: 3_000,
    series_per_write: 1,
    shape: Shape::Conj,
    duration: Duration::from_secs(3),
};

const READ_HEAVY: Config = Config {
    name: "read_heavy_algebra",
    seed_series: 50_000,
    hot_set: 10_000,
    hot_writers: 1,
    hot_ops: 5_000,
    mixed_writers: 0,
    mixed_ops: 0,
    mixed_churn: 0,
    leak: 0,
    cold_writers: 1,
    cold_ops: 2_000,
    readers: 14,
    read_ops: 1_500,
    series_per_write: 1,
    shape: Shape::Neg,
    duration: Duration::from_secs(3),
};

const STEADY_SCRAPE: Config = Config {
    name: "steady_scrape",
    seed_series: 200_000,
    hot_set: 10_000,
    hot_writers: 6,
    hot_ops: 30_000,
    mixed_writers: 0,
    mixed_ops: 0,
    mixed_churn: 0,
    leak: 0,
    cold_writers: 0,
    cold_ops: 0,
    readers: 4,
    read_ops: 5_000,
    series_per_write: 1,
    shape: Shape::Conj,
    duration: Duration::from_secs(3),
};

const SCRAPE_BATCH: Config = Config {
    name: "scrape_batch",
    seed_series: 200_000,
    hot_set: 10_000,
    hot_writers: 6,
    hot_ops: 3_000,
    mixed_writers: 0,
    mixed_ops: 0,
    mixed_churn: 0,
    leak: 0,
    cold_writers: 0,
    cold_ops: 0,
    readers: 4,
    read_ops: 5_000,
    series_per_write: 200,
    shape: Shape::Conj,
    duration: Duration::from_secs(3),
};

const PROD_SCRAPE: Config = Config {
    name: "prod_scrape",
    seed_series: 300_000,
    hot_set: 100_000,
    hot_writers: 6,
    hot_ops: 1_000,
    mixed_writers: 0,
    mixed_ops: 0,
    mixed_churn: 0,
    leak: 0,
    cold_writers: 0,
    cold_ops: 0,
    readers: 10,
    read_ops: 5_000,
    series_per_write: 500,
    shape: Shape::Conj,
    duration: Duration::from_secs(3),
};

const PROD_ROLES: Config = Config {
    name: "prod_roles",
    seed_series: 300_000,
    hot_set: 100_000,
    hot_writers: 3,
    hot_ops: 1_000,
    mixed_writers: 3,
    mixed_ops: 1_000,
    mixed_churn: 25,
    leak: 0,
    cold_writers: 0,
    cold_ops: 0,
    readers: 10,
    read_ops: 5_000,
    series_per_write: 500,
    shape: Shape::Conj,
    duration: Duration::from_secs(3),
};

const PROD_LEAK: Config = Config {
    name: "prod_leak",
    seed_series: 300_000,
    hot_set: 100_000,
    hot_writers: 3,
    hot_ops: 1_000,
    mixed_writers: 3,
    mixed_ops: 1_000,
    mixed_churn: 50,
    leak: 5,
    cold_writers: 0,
    cold_ops: 0,
    readers: 10,
    read_ops: 5_000,
    series_per_write: 500,
    shape: Shape::Conj,
    duration: Duration::from_secs(3),
};

const COLD_CHURN: Config = Config {
    name: "cold_churn",
    seed_series: 1_000,
    hot_set: 0,
    hot_writers: 0,
    hot_ops: 0,
    mixed_writers: 0,
    mixed_ops: 0,
    mixed_churn: 0,
    leak: 0,
    cold_writers: 8,
    cold_ops: 4_000,
    readers: 4,
    read_ops: 4_000,
    series_per_write: 1,
    shape: Shape::Point,
    duration: Duration::from_secs(3),
};

const BATCHED_INGEST: Config = Config {
    name: "batched_ingest",
    seed_series: 1_000,
    hot_set: 1_000,
    hot_writers: 2,
    hot_ops: 2_000,
    mixed_writers: 0,
    mixed_ops: 0,
    mixed_churn: 0,
    leak: 0,
    cold_writers: 6,
    cold_ops: 2_000,
    readers: 4,
    read_ops: 4_000,
    series_per_write: 100,
    shape: Shape::Point,
    duration: Duration::from_secs(3),
};

const FIXED_OP_CONFIGS: &[Config] = &[
    SEEDED_MIXED,
    READ_HEAVY,
    STEADY_SCRAPE,
    SCRAPE_BATCH,
    PROD_SCRAPE,
    PROD_ROLES,
    PROD_LEAK,
];
const ALL_CONFIGS: &[Config] = &[
    SEEDED_MIXED,
    READ_HEAVY,
    STEADY_SCRAPE,
    SCRAPE_BATCH,
    PROD_SCRAPE,
    PROD_ROLES,
    PROD_LEAK,
    COLD_CHURN,
    BATCHED_INGEST,
];

// workload builders
fn seed_db(db: &dyn Target, cfg: &Config) {
    let series = (0..cfg.seed_series)
        .map(|i| (seeded_series_labels(i), vec![Sample::new(0, 0.0)]))
        .collect();
    db.seed(WriteBatch { series }).expect("seed");
}

fn build_hot_set(cfg: &Config) -> Vec<LabelSet> {
    (0..cfg.hot_set).map(seeded_series_labels).collect()
}

fn hot_batch(r: &mut SmallRng, hot_set: &[LabelSet], spw: usize, t: u64) -> WriteBatch {
    let len = hot_set.len();
    let base = r.random_range(0..len);
    WriteBatch {
        series: (0..spw)
            .map(|j| {
                (
                    hot_set[(base + j) % len].clone(),
                    vec![Sample::new(t, t as f64)],
                )
            })
            .collect(),
    }
}

/// (spw - window) seeded-hot series plus a sliding window of `window` owned
/// churn series starting at id `t * leak`, so `leak` new series enter each op
/// (leak 0 = fixed window: created once, then re-appended forever).
fn mixed_batch(
    r: &mut SmallRng,
    hot_set: &[LabelSet],
    spw: usize,
    window: usize,
    leak: usize,
    writer: usize,
    t: u64,
) -> WriteBatch {
    let n_churn = window.min(spw);
    let n_hot = spw - n_churn;
    let len = hot_set.len();
    let base = r.random_range(0..len);
    let mut series = Vec::with_capacity(spw);
    for j in 0..n_hot {
        series.push((
            hot_set[(base + j) % len].clone(),
            vec![Sample::new(t, t as f64)],
        ));
    }
    let window_start = t * leak as u64;
    for slot in 0..n_churn as u64 {
        series.push((
            churn_labels(writer, window_start + slot),
            vec![Sample::new(t, t as f64)],
        ));
    }
    WriteBatch { series }
}

fn cold_batch(writer: usize, op: u64, spw: usize) -> WriteBatch {
    let base = op * spw as u64;
    WriteBatch {
        series: (0..spw)
            .map(|j| {
                (
                    cold_series_labels(writer, base + j as u64),
                    vec![Sample::new(0, 0.0)],
                )
            })
            .collect(),
    }
}

/// One row per DB:
///   <db>  <elapsed>s  writes <n>/s  series <n>/s  reads <n>/s  errs <n>
///     writes/s  = write() calls per second (each call ingests series_per_write series)
///     series/s  = writes/s * series_per_write (series-append operations per second)
///     reads/s   = successful query() calls per second
///     errs      = query() calls that returned Err (0 here; the store is stubbed)
fn report(name: &str, cfg: &Config, secs: f64, writes: u64, reads: u64, errs: u64) {
    let series = writes * cfg.series_per_write as u64;
    println!(
        "  {:<13} {:>7.3}s  writes {:>9.0}/s  series {:>10.0}/s  reads {:>9.0}/s  errs {}",
        name,
        secs,
        writes as f64 / secs,
        series as f64 / secs,
        reads as f64 / secs,
        errs,
    );
}

/// Header line for a config:
///   == <kind> :: <name> (hot H, mixed MxW leak L, cold C, readers R, spw S) ==
///     hot/mixed/cold/readers = thread counts per role
///     MxW  = mixed_writers x mixed_churn (window size); leak = new series/op/writer
///     spw  = series_per_write (series per write() call)
fn header(kind: &str, cfg: &Config) {
    println!(
        "== {} :: {} (hot {}, mixed {}x{} leak {}, cold {}, readers {}, spw {}) ==",
        kind,
        cfg.name,
        cfg.hot_writers,
        cfg.mixed_writers,
        cfg.mixed_churn,
        cfg.leak,
        cfg.cold_writers,
        cfg.readers,
        cfg.series_per_write,
    );
}

//  Fixed-op harness
fn run_fixed_op(db: &dyn Target, cfg: &Config) -> (Duration, u64) {
    seed_db(db, cfg);
    debug_assert!(
        (cfg.hot_writers == 0 && cfg.mixed_writers == 0) || cfg.hot_set > 0,
        "hot/mixed writers need a hot set"
    );
    let hot_set = build_hot_set(cfg);
    let hot_set = &hot_set;
    let n = cfg.hot_writers + cfg.mixed_writers + cfg.cold_writers + cfg.readers;
    let barrier = Barrier::new(n + 1);
    let barrier = &barrier;
    let full = TimeRange::new(0, u64::MAX);

    std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(n);

        for w in 0..cfg.hot_writers {
            handles.push(s.spawn(move || {
                let mut r = rng(0x100 + w as u64);
                barrier.wait();
                for t in 0..cfg.hot_ops {
                    db.write(hot_batch(&mut r, hot_set, cfg.series_per_write, t))
                        .unwrap();
                }
                0u64
            }));
        }
        for w in 0..cfg.mixed_writers {
            handles.push(s.spawn(move || {
                let mut r = rng(0x300 + w as u64);
                barrier.wait();
                for t in 0..cfg.mixed_ops {
                    db.write(mixed_batch(
                        &mut r,
                        hot_set,
                        cfg.series_per_write,
                        cfg.mixed_churn,
                        cfg.leak,
                        w,
                        t,
                    ))
                    .unwrap();
                }
                0u64
            }));
        }
        for w in 0..cfg.cold_writers {
            handles.push(s.spawn(move || {
                barrier.wait();
                for op in 0..cfg.cold_ops {
                    db.write(cold_batch(w, op, cfg.series_per_write)).unwrap();
                }
                0u64
            }));
        }
        for rd in 0..cfg.readers {
            handles.push(s.spawn(move || {
                let mut r = rng(0x200 + rd as u64);
                let mut errs = 0u64;
                barrier.wait();
                for _ in 0..cfg.read_ops {
                    let m = build_query(cfg.shape, &mut r);
                    match db.query(&m, full) {
                        Ok(res) => {
                            black_box(res);
                        }
                        Err(_) => errs += 1,
                    }
                }
                errs
            }));
        }

        barrier.wait();
        let start = Instant::now();
        let errs: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();
        (start.elapsed(), errs)
    })
}

fn report_fixed_op(name: &str, cfg: &Config, (elapsed, errs): (Duration, u64)) {
    let writes = cfg.hot_writers as u64 * cfg.hot_ops
        + cfg.mixed_writers as u64 * cfg.mixed_ops
        + cfg.cold_writers as u64 * cfg.cold_ops;
    let reads = cfg.readers as u64 * cfg.read_ops;
    report(name, cfg, elapsed.as_secs_f64(), writes, reads, errs);
}

/// Throughput with a fixed op count per worker. Each worker runs a set number of
/// ops; elapsed = wall-clock from barrier release to the last worker's join, and
/// the /s figures divide the (analytic) total op counts by that elapsed. A role
/// that finishes early stops contending, so this slightly flatters the fastest
/// DB. Runs FIXED_OP_CONFIGS only — cold-heavy configs would force the
/// create-path quadratic to completion. Columns: see `report`.
#[test]
#[ignore = "manual benchmark"]
fn bench_fixed_op() {
    for cfg in FIXED_OP_CONFIGS {
        header("fixed_op", cfg);
        report_fixed_op(
            "single_lock",
            cfg,
            run_fixed_op(&SingleLockIndexDb::new(), cfg),
        );
        report_fixed_op(
            "batch_single",
            cfg,
            run_fixed_op(&BatchedSingleLockDb::new(), cfg),
        );
        report_fixed_op(
            "concurrent",
            cfg,
            run_fixed_op(&ConcurrentIndexDb::new(), cfg),
        );
        report_fixed_op("batched", cfg, run_fixed_op(&BatchedIndexDb::new(), cfg));
        println!();
    }
}

//  Fixed-duration harness
struct DurReport {
    elapsed: Duration,
    writes: u64,
    reads: u64,
    read_errs: u64,
}

fn run_fixed_duration(db: &dyn Target, cfg: &Config) -> DurReport {
    seed_db(db, cfg);
    debug_assert!(
        (cfg.hot_writers == 0 && cfg.mixed_writers == 0) || cfg.hot_set > 0,
        "hot/mixed writers need a hot set"
    );
    let hot_set = build_hot_set(cfg);
    let hot_set = &hot_set;
    let writes = AtomicU64::new(0);
    let reads = AtomicU64::new(0);
    let read_errs = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    let n = cfg.hot_writers + cfg.mixed_writers + cfg.cold_writers + cfg.readers;
    let barrier = Barrier::new(n + 1);
    let full = TimeRange::new(0, u64::MAX);

    let (writes, reads, read_errs, stop, barrier) = (&writes, &reads, &read_errs, &stop, &barrier);

    let elapsed = std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(n);

        for w in 0..cfg.hot_writers {
            handles.push(s.spawn(move || {
                let mut r = rng(0x100 + w as u64);
                let mut local = 0u64;
                barrier.wait();
                while !stop.load(Relaxed) {
                    db.write(hot_batch(&mut r, hot_set, cfg.series_per_write, local))
                        .unwrap();
                    local += 1;
                }
                writes.fetch_add(local, Relaxed);
            }));
        }
        for w in 0..cfg.mixed_writers {
            handles.push(s.spawn(move || {
                let mut r = rng(0x300 + w as u64);
                let mut local = 0u64;
                barrier.wait();
                while !stop.load(Relaxed) {
                    db.write(mixed_batch(
                        &mut r,
                        hot_set,
                        cfg.series_per_write,
                        cfg.mixed_churn,
                        cfg.leak,
                        w,
                        local,
                    ))
                    .unwrap();
                    local += 1;
                }
                writes.fetch_add(local, Relaxed);
            }));
        }
        for w in 0..cfg.cold_writers {
            handles.push(s.spawn(move || {
                let mut local = 0u64;
                barrier.wait();
                while !stop.load(Relaxed) {
                    db.write(cold_batch(w, local, cfg.series_per_write))
                        .unwrap();
                    local += 1;
                }
                writes.fetch_add(local, Relaxed);
            }));
        }
        for rd in 0..cfg.readers {
            handles.push(s.spawn(move || {
                let mut r = rng(0x200 + rd as u64);
                let (mut ok, mut err) = (0u64, 0u64);
                barrier.wait();
                while !stop.load(Relaxed) {
                    let m = build_query(cfg.shape, &mut r);
                    match db.query(&m, full) {
                        Ok(res) => {
                            black_box(res);
                            ok += 1;
                        }
                        Err(_) => err += 1,
                    }
                }
                reads.fetch_add(ok, Relaxed);
                read_errs.fetch_add(err, Relaxed);
            }));
        }

        barrier.wait();
        let start = Instant::now();
        std::thread::sleep(cfg.duration);
        stop.store(true, Relaxed);
        for h in handles {
            h.join().unwrap();
        }
        start.elapsed()
    });

    DurReport {
        elapsed,
        writes: writes.load(Relaxed),
        reads: reads.load(Relaxed),
        read_errs: read_errs.load(Relaxed),
    }
}

/// Throughput over a fixed wall-clock window. Every role contends for the whole
/// window (fair contention); each worker counts locally and adds once at the end.
/// On a growing index (leak/cold configs), later ops are genuinely slower, so the
/// /s figures are the window average including that drift. Runs ALL_CONFIGS.
/// Columns: see `report`.
#[test]
#[ignore = "manual benchmark"]
fn bench_fixed_duration() {
    for cfg in ALL_CONFIGS {
        header("fixed_duration", cfg);
        for (name, make) in [
            (
                "single_lock",
                (|| Box::new(SingleLockIndexDb::new()) as Box<dyn Target>)
                    as fn() -> Box<dyn Target>,
            ),
            ("batch_single", || {
                Box::new(BatchedSingleLockDb::new()) as Box<dyn Target>
            }),
            ("concurrent", || {
                Box::new(ConcurrentIndexDb::new()) as Box<dyn Target>
            }),
            ("batched", || {
                Box::new(BatchedIndexDb::new()) as Box<dyn Target>
            }),
        ] {
            let db = make();
            let rep = run_fixed_duration(db.as_ref(), cfg);
            report(
                name,
                cfg,
                rep.elapsed.as_secs_f64(),
                rep.writes,
                rep.reads,
                rep.read_errs,
            );
        }
        println!();
    }
}

//  Tokio harness — inline + yield
const YIELD_EVERY: u64 = 64;

fn run_tokio(db: Arc<dyn Target>, cfg: &Config, rt: &Runtime) -> (Duration, u64) {
    seed_db(db.as_ref(), cfg);
    let hot_set = Arc::new(build_hot_set(cfg));
    let n = cfg.hot_writers + cfg.mixed_writers + cfg.cold_writers + cfg.readers;
    let full = TimeRange::new(0, u64::MAX);
    let cfg = *cfg;

    rt.block_on(async move {
        let barrier = Arc::new(tokio::sync::Barrier::new(n + 1));
        let mut handles = Vec::with_capacity(n);

        for w in 0..cfg.hot_writers {
            let (db, barrier, hot_set) = (db.clone(), barrier.clone(), hot_set.clone());
            handles.push(tokio::spawn(async move {
                let mut r = rng(0x100 + w as u64);
                barrier.wait().await;
                for t in 0..cfg.hot_ops {
                    db.write(hot_batch(&mut r, &hot_set, cfg.series_per_write, t))
                        .unwrap();
                    if t % YIELD_EVERY == 0 {
                        tokio::task::yield_now().await;
                    }
                }
                0u64
            }));
        }
        for w in 0..cfg.mixed_writers {
            let (db, barrier, hot_set) = (db.clone(), barrier.clone(), hot_set.clone());
            handles.push(tokio::spawn(async move {
                let mut r = rng(0x300 + w as u64);
                barrier.wait().await;
                for t in 0..cfg.mixed_ops {
                    db.write(mixed_batch(
                        &mut r,
                        &hot_set,
                        cfg.series_per_write,
                        cfg.mixed_churn,
                        cfg.leak,
                        w,
                        t,
                    ))
                    .unwrap();
                    if t % YIELD_EVERY == 0 {
                        tokio::task::yield_now().await;
                    }
                }
                0u64
            }));
        }
        for w in 0..cfg.cold_writers {
            let (db, barrier) = (db.clone(), barrier.clone());
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                for op in 0..cfg.cold_ops {
                    db.write(cold_batch(w, op, cfg.series_per_write)).unwrap();
                    if op % YIELD_EVERY == 0 {
                        tokio::task::yield_now().await;
                    }
                }
                0u64
            }));
        }
        for rd in 0..cfg.readers {
            let (db, barrier) = (db.clone(), barrier.clone());
            handles.push(tokio::spawn(async move {
                let mut r = rng(0x200 + rd as u64);
                let mut errs = 0u64;
                barrier.wait().await;
                for k in 0..cfg.read_ops {
                    let m = build_query(cfg.shape, &mut r);
                    match db.query(&m, full) {
                        Ok(res) => {
                            black_box(res);
                        }
                        Err(_) => errs += 1,
                    }
                    if k % YIELD_EVERY == 0 {
                        tokio::task::yield_now().await;
                    }
                }
                errs
            }));
        }

        barrier.wait().await;
        let start = Instant::now();
        let mut errs = 0u64;
        for h in handles {
            errs += h.await.unwrap();
        }
        (start.elapsed(), errs)
    })
}

fn run_tokio_blocking(db: Arc<dyn Target>, cfg: &Config, rt: &Runtime) -> (Duration, u64) {
    seed_db(db.as_ref(), cfg);
    let hot_set = Arc::new(build_hot_set(cfg));
    let n = cfg.hot_writers + cfg.mixed_writers + cfg.cold_writers + cfg.readers;
    let full = TimeRange::new(0, u64::MAX);
    let cfg = *cfg;

    rt.block_on(async move {
        let barrier = Arc::new(tokio::sync::Barrier::new(n + 1));
        let mut handles = Vec::with_capacity(n);

        for w in 0..cfg.hot_writers {
            let (db, barrier, hot_set) = (db.clone(), barrier.clone(), hot_set.clone());
            handles.push(tokio::spawn(async move {
                let mut r = rng(0x100 + w as u64);
                barrier.wait().await;
                for t in 0..cfg.hot_ops {
                    let batch = hot_batch(&mut r, &hot_set, cfg.series_per_write, t);
                    let db2 = db.clone();
                    tokio::task::spawn_blocking(move || db2.write(batch).unwrap())
                        .await
                        .unwrap();
                }
                0u64
            }));
        }
        for w in 0..cfg.mixed_writers {
            let (db, barrier, hot_set) = (db.clone(), barrier.clone(), hot_set.clone());
            handles.push(tokio::spawn(async move {
                let mut r = rng(0x300 + w as u64);
                barrier.wait().await;
                for t in 0..cfg.mixed_ops {
                    let batch = mixed_batch(
                        &mut r,
                        &hot_set,
                        cfg.series_per_write,
                        cfg.mixed_churn,
                        cfg.leak,
                        w,
                        t,
                    );
                    let db2 = db.clone();
                    tokio::task::spawn_blocking(move || db2.write(batch).unwrap())
                        .await
                        .unwrap();
                }
                0u64
            }));
        }
        for w in 0..cfg.cold_writers {
            let (db, barrier) = (db.clone(), barrier.clone());
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                for op in 0..cfg.cold_ops {
                    let batch = cold_batch(w, op, cfg.series_per_write);
                    let db2 = db.clone();
                    tokio::task::spawn_blocking(move || db2.write(batch).unwrap())
                        .await
                        .unwrap();
                }
                0u64
            }));
        }
        for rd in 0..cfg.readers {
            let (db, barrier) = (db.clone(), barrier.clone());
            handles.push(tokio::spawn(async move {
                let mut r = rng(0x200 + rd as u64);
                let mut errs = 0u64;
                barrier.wait().await;
                for _ in 0..cfg.read_ops {
                    let m = build_query(cfg.shape, &mut r);
                    let db2 = db.clone();
                    let res = tokio::task::spawn_blocking(move || db2.query(&m, full))
                        .await
                        .unwrap();
                    match res {
                        Ok(rr) => {
                            black_box(rr);
                        }
                        Err(_) => errs += 1,
                    }
                }
                errs
            }));
        }

        barrier.wait().await;
        let start = Instant::now();
        let mut errs = 0u64;
        for h in handles {
            errs += h.await.unwrap();
        }
        (start.elapsed(), errs)
    })
}

fn multi_thread_rt() -> Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(8),
        )
        .enable_all()
        .build()
        .unwrap()
}

/// PROD_LEAK workload driven on a tokio multi-thread runtime, in two modes:
/// "inline + yield" calls the (synchronous) DB directly on the runtime workers
/// with a periodic yield_now; "spawn_blocking" offloads every DB call to the
/// blocking pool. Compare the two to see the offload overhead for short ops.
/// Columns: see `report` (same throughput format as fixed_op).
#[test]
#[ignore = "manual benchmark"]
fn bench_tokio() {
    let rt = multi_thread_rt();
    let cfg = PROD_LEAK;

    println!("== tokio (inline + yield) :: {} ==", cfg.name);
    report_fixed_op(
        "single_lock",
        &cfg,
        run_tokio(Arc::new(SingleLockIndexDb::new()), &cfg, &rt),
    );
    report_fixed_op(
        "batch_single",
        &cfg,
        run_tokio(Arc::new(BatchedSingleLockDb::new()), &cfg, &rt),
    );
    report_fixed_op(
        "concurrent",
        &cfg,
        run_tokio(Arc::new(ConcurrentIndexDb::new()), &cfg, &rt),
    );
    report_fixed_op(
        "batched",
        &cfg,
        run_tokio(Arc::new(BatchedIndexDb::new()), &cfg, &rt),
    );
    println!();

    println!("== tokio (spawn_blocking) :: {} ==", cfg.name);
    report_fixed_op(
        "single_lock",
        &cfg,
        run_tokio_blocking(Arc::new(SingleLockIndexDb::new()), &cfg, &rt),
    );
    report_fixed_op(
        "batch_single",
        &cfg,
        run_tokio_blocking(Arc::new(BatchedSingleLockDb::new()), &cfg, &rt),
    );
    report_fixed_op(
        "concurrent",
        &cfg,
        run_tokio_blocking(Arc::new(ConcurrentIndexDb::new()), &cfg, &rt),
    );
    report_fixed_op(
        "batched",
        &cfg,
        run_tokio_blocking(Arc::new(BatchedIndexDb::new()), &cfg, &rt),
    );
    println!();
}

// Original benchmark shape
/// Legacy shape: 8 writers each creating unique single-series batches, 56 readers
/// repeatedly querying one static 1-element posting list, 1000 ops each, on tokio.
/// Row: `<db>  <elapsed>s  <n> ops/s` where ops = (writers + readers) * OPS total
/// operations divided by elapsed.
#[test]
#[ignore = "manual benchmark"]
fn bench_original_shape() {
    const WRITERS: usize = 8;
    const READERS: usize = 56;
    const OPS: u64 = 1_000;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .unwrap();

    fn run(db: Arc<dyn Target>, rt: &Runtime) -> Duration {
        db.seed(WriteBatch {
            series: vec![(
                LabelSet::from_labels([
                    Label::new("__name__", "seed"),
                    Label::new("kind", "stable"),
                ]),
                vec![Sample::new(0, 0.0)],
            )],
        })
        .unwrap();
        let full = TimeRange::new(0, u64::MAX);

        rt.block_on(async move {
            let barrier = Arc::new(tokio::sync::Barrier::new(WRITERS + READERS + 1));
            let mut handles = Vec::new();

            for w in 0..WRITERS {
                let (db, barrier) = (db.clone(), barrier.clone());
                handles.push(tokio::spawn(async move {
                    barrier.wait().await;
                    for n in 0..OPS {
                        db.write(WriteBatch {
                            series: vec![(
                                LabelSet::from_labels([
                                    Label::new("__name__", "load"),
                                    Label::new("series", format!("{w}-{n}")),
                                ]),
                                vec![Sample::new(n, n as f64)],
                            )],
                        })
                        .unwrap();
                        if n % YIELD_EVERY == 0 {
                            tokio::task::yield_now().await;
                        }
                    }
                }));
            }
            for _ in 0..READERS {
                let (db, barrier) = (db.clone(), barrier.clone());
                handles.push(tokio::spawn(async move {
                    let m = [Matcher::new("kind", "stable", Equal)];
                    barrier.wait().await;
                    for n in 0..OPS {
                        let res = db.query(&m, full).unwrap();
                        black_box(res);
                        if n % YIELD_EVERY == 0 {
                            tokio::task::yield_now().await;
                        }
                    }
                }));
            }

            barrier.wait().await;
            let start = Instant::now();
            for h in handles {
                h.await.unwrap();
            }
            start.elapsed()
        })
    }

    println!("== original_shape (8 writers, 56 readers, {OPS} ops) ==");
    for (name, e) in [
        ("single_lock", run(Arc::new(SingleLockIndexDb::new()), &rt)),
        (
            "batch_single",
            run(Arc::new(BatchedSingleLockDb::new()), &rt),
        ),
        ("concurrent", run(Arc::new(ConcurrentIndexDb::new()), &rt)),
        ("batched", run(Arc::new(BatchedIndexDb::new()), &rt)),
    ] {
        let total = (WRITERS + READERS) as f64 * OPS as f64;
        println!(
            "  {:<13} {:>7.3}s  {:>10.0} ops/s",
            name,
            e.as_secs_f64(),
            total / e.as_secs_f64()
        );
    }
}

// Paced latency harness
#[derive(Clone, Copy)]
struct PacedConfig {
    name: &'static str,
    seed_series: u64,
    hot_set: u64,
    writers: usize,
    scrape_rate: f64,
    series_per_scrape: usize,
    mixed_churn: usize,
    leak: usize,
    readers: usize,
    query_rate: f64,
    shape: Shape,
    duration: Duration,
    warmup: Duration,
}

const PROD_PACED: PacedConfig = PacedConfig {
    name: "prod_paced",
    seed_series: 300_000,
    hot_set: 100_000,
    writers: 6,
    scrape_rate: 300.0,
    series_per_scrape: 500,
    mixed_churn: 10,
    leak: 0,
    readers: 10,
    query_rate: 200.0,
    shape: Shape::Conj,
    duration: Duration::from_secs(5),
    warmup: Duration::from_secs(2),
};

const PROD_PACED_LEAK: PacedConfig = PacedConfig {
    name: "prod_paced_leak",
    seed_series: 300_000,
    hot_set: 100_000,
    writers: 6,
    scrape_rate: 300.0,
    series_per_scrape: 500,
    mixed_churn: 30,
    leak: 3,
    readers: 10,
    query_rate: 200.0,
    shape: Shape::Conj,
    duration: Duration::from_secs(5),
    warmup: Duration::from_secs(2),
};

const PROD_PACED_HEAVY: PacedConfig = PacedConfig {
    name: "prod_paced_heavy",
    seed_series: 300_000,
    hot_set: 100_000,
    writers: 6,
    scrape_rate: 1_500.0,
    series_per_scrape: 500,
    mixed_churn: 30,
    leak: 3,
    readers: 10,
    query_rate: 1_000.0,
    shape: Shape::Conj,
    duration: Duration::from_secs(5),
    warmup: Duration::from_secs(2),
};

const PACED_CONFIGS: &[PacedConfig] = &[PROD_PACED, PROD_PACED_LEAK, PROD_PACED_HEAVY];

fn seed_n(db: &dyn Target, n: u64) {
    let series = (0..n)
        .map(|i| (seeded_series_labels(i), vec![Sample::new(0, 0.0)]))
        .collect();
    db.seed(WriteBatch { series }).expect("seed");
}

/// Returns (count, p50, p90, p99, p999, max) over the samples, in microseconds.
fn percentiles(mut v: Vec<u64>) -> (usize, u64, u64, u64, u64, u64) {
    if v.is_empty() {
        return (0, 0, 0, 0, 0, 0);
    }
    v.sort_unstable();
    let n = v.len();
    let at = |p: f64| v[(((n as f64 - 1.0) * p).round() as usize).min(n - 1)];
    (n, at(0.50), at(0.90), at(0.99), at(0.999), v[n - 1])
}

fn run_paced(db: &dyn Target, cfg: &PacedConfig) -> (Vec<u64>, Vec<u64>) {
    seed_n(db, cfg.seed_series);
    let hot: Vec<LabelSet> = (0..cfg.hot_set).map(seeded_series_labels).collect();
    let hot = &hot;

    let n = cfg.writers + cfg.readers;
    let barrier = Barrier::new(n + 1);
    let barrier = &barrier;
    let full = TimeRange::new(0, u64::MAX);

    let w_period = Duration::from_secs_f64(cfg.writers as f64 / cfg.scrape_rate);
    let r_period = Duration::from_secs_f64(cfg.readers as f64 / cfg.query_rate);

    std::thread::scope(|s| {
        let mut wh = Vec::with_capacity(cfg.writers);
        let mut rh = Vec::with_capacity(cfg.readers);

        for w in 0..cfg.writers {
            let offset = w_period.mul_f64(w as f64 / cfg.writers as f64);
            wh.push(s.spawn(move || {
                let mut rr = rng(0x100 + w as u64);
                let cap = (cfg.duration.as_secs_f64() / w_period.as_secs_f64()) as usize + 16;
                let mut lat = Vec::with_capacity(cap);
                barrier.wait();
                let start = Instant::now();
                let (end, rec_after) = (start + cfg.duration, start + cfg.warmup);
                let mut k = 0u64;
                loop {
                    let now = Instant::now();
                    if now >= end {
                        break;
                    }
                    let scheduled = start + offset + w_period.mul_f64(k as f64);
                    if scheduled > now {
                        std::thread::sleep(scheduled - now);
                    }
                    let batch = if cfg.mixed_churn > 0 {
                        mixed_batch(
                            &mut rr,
                            hot,
                            cfg.series_per_scrape,
                            cfg.mixed_churn,
                            cfg.leak,
                            w,
                            k,
                        )
                    } else {
                        hot_batch(&mut rr, hot, cfg.series_per_scrape, k)
                    };
                    db.write(batch).unwrap();
                    if scheduled >= rec_after {
                        lat.push(
                            Instant::now()
                                .saturating_duration_since(scheduled)
                                .as_micros() as u64,
                        );
                    }
                    k += 1;
                }
                lat
            }));
        }

        for rd in 0..cfg.readers {
            let offset = r_period.mul_f64(rd as f64 / cfg.readers as f64);
            rh.push(s.spawn(move || {
                let mut rr = rng(0x200 + rd as u64);
                let cap = (cfg.duration.as_secs_f64() / r_period.as_secs_f64()) as usize + 16;
                let mut lat = Vec::with_capacity(cap);
                barrier.wait();
                let start = Instant::now();
                let (end, rec_after) = (start + cfg.duration, start + cfg.warmup);
                let mut k = 0u64;
                loop {
                    let now = Instant::now();
                    if now >= end {
                        break;
                    }
                    let scheduled = start + offset + r_period.mul_f64(k as f64);
                    if scheduled > now {
                        std::thread::sleep(scheduled - now);
                    }
                    black_box(db.query(&build_query(cfg.shape, &mut rr), full).unwrap());
                    if scheduled >= rec_after {
                        lat.push(
                            Instant::now()
                                .saturating_duration_since(scheduled)
                                .as_micros() as u64,
                        );
                    }
                    k += 1;
                }
                lat
            }));
        }

        barrier.wait();
        let mut writes = Vec::new();
        for h in wh {
            writes.extend(h.join().unwrap());
        }
        let mut reads = Vec::new();
        for h in rh {
            reads.extend(h.join().unwrap());
        }
        (writes, reads)
    })
}

/// Two rows per DB — one for writes, one for reads:
///   <db> writes  off <n>/s got <n>/s  lat(µs) p50 .. p90 .. p99 .. p999 .. max ..  n <count>
///        reads   off <n>/s got <n>/s  lat(µs) p50 .. ...                            n <count>
///     off     = offered rate (ops/s the harness scheduled)
///     got     = achieved rate (recorded ops / measurement window); got << off
///               means the DB could not keep up with the offered load
///     lat(µs) = per-op latency percentiles in microseconds, measured from each
///               op's scheduled tick (coordinated-omission correct: a late op
///               reports its full delay rather than hiding it)
///     n       = latency samples recorded (after warmup)
fn report_paced(name: &str, cfg: &PacedConfig, (writes, reads): (Vec<u64>, Vec<u64>)) {
    let secs = (cfg.duration - cfg.warmup).as_secs_f64();
    let (wc, w50, w90, w99, w999, wmax) = percentiles(writes);
    let (rc, r50, r90, r99, r999, rmax) = percentiles(reads);
    println!(
        "  {:<13} writes  off {:>6.0}/s got {:>6.0}/s  lat(µs) p50 {:>5} p90 {:>6} p99 {:>7} p999 {:>8} max {:>8}  n {}",
        name,
        cfg.scrape_rate,
        wc as f64 / secs,
        w50,
        w90,
        w99,
        w999,
        wmax,
        wc
    );
    println!(
        "  {:<13} reads   off {:>6.0}/s got {:>6.0}/s  lat(µs) p50 {:>5} p90 {:>6} p99 {:>7} p999 {:>8} max {:>8}  n {}",
        "",
        cfg.query_rate,
        rc as f64 / secs,
        r50,
        r90,
        r99,
        r999,
        rmax,
        rc
    );
}

/// Latency under a fixed OFFERED load (not max throughput). Writers fire at a
/// paced rate (staggered so scrapes stream), readers query at a paced rate; the
/// first `warmup` is discarded. Columns: see `report_paced`.
#[test]
#[ignore = "manual benchmark"]
fn bench_paced() {
    for cfg in PACED_CONFIGS {
        println!(
            "== paced :: {} (writers {} @ {:.0}/s x{} churn {} leak {}, readers {} @ {:.0}/s) ==",
            cfg.name,
            cfg.writers,
            cfg.scrape_rate,
            cfg.series_per_scrape,
            cfg.mixed_churn,
            cfg.leak,
            cfg.readers,
            cfg.query_rate
        );
        report_paced(
            "single_lock",
            cfg,
            run_paced(&SingleLockIndexDb::new(), cfg),
        );
        report_paced(
            "batch_single",
            cfg,
            run_paced(&BatchedSingleLockDb::new(), cfg),
        );
        report_paced("concurrent", cfg, run_paced(&ConcurrentIndexDb::new(), cfg));
        report_paced("batched", cfg, run_paced(&BatchedIndexDb::new(), cfg));
        println!();
    }
}

// Leak sweep — find the churn rate where copy-on-write loses to mutate-in-place
/// Sweeps `leak` (new series/op/writer) over LEAK_SWEEP with all else pinned,
/// fixed-duration. Read the series/s column: the crossover is where the
/// copy-on-write DBs (concurrent, batched) drop below mutate-in-place
/// (single_lock). The fixed window captures within-run slowdown as the index
/// grows. `mixed_churn` is raised to 400 so a high leak still keeps each churn
/// series alive >1 op (window > leak) rather than saturating to all-new.
/// Columns: see `report`.
#[test]
#[ignore = "manual benchmark"]
fn bench_leak_sweep() {
    let base = Config {
        mixed_churn: 400,
        ..PROD_LEAK
    };
    for &leak in LEAK_SWEEP {
        let cfg = leak_config(base, leak);
        println!(
            "== leak_sweep :: leak {} (window {}) ==",
            leak, cfg.mixed_churn
        );
        for (name, make) in [
            (
                "single_lock",
                (|| Box::new(SingleLockIndexDb::new()) as Box<dyn Target>)
                    as fn() -> Box<dyn Target>,
            ),
            ("batch_single", || {
                Box::new(BatchedSingleLockDb::new()) as Box<dyn Target>
            }),
            ("concurrent", || {
                Box::new(ConcurrentIndexDb::new()) as Box<dyn Target>
            }),
            ("batched", || {
                Box::new(BatchedIndexDb::new()) as Box<dyn Target>
            }),
        ] {
            let db = make();
            let r = run_fixed_duration(db.as_ref(), &cfg);
            report(
                name,
                &cfg,
                r.elapsed.as_secs_f64(),
                r.writes,
                r.reads,
                r.read_errs,
            );
        }
        println!();
    }
}

/// Fixed-op counterpart to bench_leak_sweep. Op counts shrink as leak rises
/// (CHURN_BUDGET new series per writer) so no point runs the create-path
/// quadratic to completion. Because ops are bounded, this measures mostly the
/// early (smaller) index — compare against bench_leak_sweep, whose window
/// average includes the growing-index slowdown. Columns: see `report`.
#[test]
#[ignore = "manual benchmark"]
fn bench_leak_sweep_fixed_op() {
    let base = Config {
        mixed_churn: 400,
        ..PROD_LEAK
    };
    const CHURN_BUDGET: u64 = 20_000; // ~new series per mixed writer over the run
    for &leak in LEAK_SWEEP {
        let ops = if leak == 0 {
            1_000
        } else {
            (CHURN_BUDGET / leak as u64).clamp(50, 2_000)
        };
        let cfg = Config {
            leak,
            mixed_ops: ops,
            hot_ops: ops,
            ..base
        };
        println!(
            "== leak_sweep_op :: leak {} (ops {}, window {}) ==",
            leak, ops, cfg.mixed_churn
        );
        report_fixed_op(
            "single_lock",
            &cfg,
            run_fixed_op(&SingleLockIndexDb::new(), &cfg),
        );
        report_fixed_op(
            "batch_single",
            &cfg,
            run_fixed_op(&BatchedSingleLockDb::new(), &cfg),
        );
        report_fixed_op(
            "concurrent",
            &cfg,
            run_fixed_op(&ConcurrentIndexDb::new(), &cfg),
        );
        report_fixed_op("batched", &cfg, run_fixed_op(&BatchedIndexDb::new(), &cfg));
        println!();
    }
}
