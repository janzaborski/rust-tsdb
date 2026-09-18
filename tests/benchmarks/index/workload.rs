use std::hint::black_box;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use super::shared::{Shape, build_query_bank, cold_series_labels, seeded_series_labels};
use super::targets::{BenchIndex, Index, Simple, Target, V10A, V10APacked, V11A, V11APacked};

const SEED_SERIES: usize = 100_000;
const REPETITIONS: usize = 5;
const COLD_PER_WRITER: usize = 40_000;
const WARMUP: Duration = Duration::from_secs(5);
const MEASURE: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy)]
enum Workload {
    Hot,
    Mixed,
    Cold,
    Steady,
    ReadHeavy,
    QueryHeavy,
}

impl Workload {
    const ALL: [Self; 6] = [
        Self::Hot,
        Self::Mixed,
        Self::Cold,
        Self::Steady,
        Self::ReadHeavy,
        Self::QueryHeavy,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Hot => "hot",
            Self::Mixed => "mixed",
            Self::Cold => "cold",
            Self::Steady => "steady",
            Self::ReadHeavy => "read_heavy",
            Self::QueryHeavy => "query_heavy",
        }
    }

    fn workers(self) -> (usize, usize) {
        match self {
            Self::Hot | Self::Mixed | Self::Cold => (8, 0),
            Self::Steady => (6, 2),
            Self::ReadHeavy => (2, 6),
            Self::QueryHeavy => (1, 7),
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Counts {
    writes: u64,
    reads: u64,
}

impl Counts {
    fn add(self, other: Self) -> Self {
        Self {
            writes: self.writes + other.writes,
            reads: self.reads + other.reads,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Rates {
    writes_s: f64,
    reads_s: f64,
    total_s: f64,
}

fn selected(target: Target) -> bool {
    std::env::var("TSDB_BENCH_TARGET").is_ok_and(|value| value != target.name())
}

fn seed<T: BenchIndex>(index: &T, labels: &[T::Labels]) {
    for labels in labels {
        black_box(index.encode(labels));
    }
}

fn hot_labels<T: BenchIndex>() -> Arc<Vec<T::Labels>> {
    Arc::new(
        (0..SEED_SERIES as u64)
            .map(seeded_series_labels)
            .map(T::native)
            .collect(),
    )
}

fn run_timed_once<T: BenchIndex>(
    index: &Arc<T>,
    hot: &Arc<Vec<T::Labels>>,
    workload: Workload,
    duration: Duration,
) -> (Counts, Duration) {
    let (writers, readers) = workload.workers();
    let queries = build_query_bank(Shape::Conj, SEED_SERIES);
    let stop = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(Barrier::new(writers + readers + 1));

    let (counts, elapsed) = std::thread::scope(|scope| {
        let writer_handles = (0..writers)
            .map(|worker| {
                let index = Arc::clone(index);
                let hot = Arc::clone(hot);
                let stop = Arc::clone(&stop);
                let barrier = Arc::clone(&barrier);

                scope.spawn(move || {
                    let mut writes = 0_u64;
                    let mut position = worker * 997;
                    barrier.wait();

                    while !stop.load(Ordering::Relaxed) {
                        position = (position + 7919) % hot.len();
                        black_box(index.encode(&hot[position]));
                        writes += 1;
                    }

                    Counts { writes, reads: 0 }
                })
            })
            .collect::<Vec<_>>();

        let reader_handles = (0..readers)
            .map(|reader| {
                let index = Arc::clone(index);
                let queries = Arc::clone(&queries);
                let stop = Arc::clone(&stop);
                let barrier = Arc::clone(&barrier);

                scope.spawn(move || {
                    let mut reads = 0_u64;
                    let mut position = reader * 613;
                    barrier.wait();

                    while !stop.load(Ordering::Relaxed) {
                        position = (position + 3571) % queries.len();
                        black_box(index.resolve(&queries[position]));
                        reads += 1;
                    }

                    Counts { writes: 0, reads }
                })
            })
            .collect::<Vec<_>>();

        barrier.wait();
        let started = Instant::now();
        std::thread::sleep(duration);
        stop.store(true, Ordering::Relaxed);

        let counts = writer_handles
            .into_iter()
            .chain(reader_handles)
            .map(|handle| handle.join().unwrap())
            .fold(Counts::default(), Counts::add);

        let elapsed = started.elapsed();
        (counts, elapsed)
    });

    (counts, elapsed)
}

fn run_timed<T: BenchIndex>(workload: Workload) -> Rates {
    let index = Arc::new(T::new());
    let hot = hot_labels::<T>();
    seed(index.as_ref(), &hot);

    black_box(run_timed_once(&index, &hot, workload, WARMUP));

    let (counts, elapsed) = run_timed_once(&index, &hot, workload, MEASURE);
    let seconds = elapsed.as_secs_f64();

    Rates {
        writes_s: counts.writes as f64 / seconds,
        reads_s: counts.reads as f64 / seconds,
        total_s: (counts.writes + counts.reads) as f64 / seconds,
    }
}

fn cold_pool<T: BenchIndex>() -> Arc<Vec<Vec<T::Labels>>> {
    Arc::new(
        (0..8)
            .map(|writer| {
                (0..COLD_PER_WRITER)
                    .map(|n| {
                        let identity = SEED_SERIES as u64 + (writer * COLD_PER_WRITER + n) as u64;
                        T::native(cold_series_labels(writer, identity))
                    })
                    .collect()
            })
            .collect(),
    )
}

fn run_bounded<T: BenchIndex>(workload: Workload) -> Rates {
    let index = Arc::new(T::new());
    let hot = hot_labels::<T>();
    seed(index.as_ref(), &hot);

    let cold = cold_pool::<T>();
    assert!(
        cold.iter()
            .flatten()
            .all(|labels| index.lookup(labels).is_none())
    );

    let barrier = Arc::new(Barrier::new(9));

    let elapsed = std::thread::scope(|scope| {
        let handles = (0..8)
            .map(|worker| {
                let index = Arc::clone(&index);
                let hot = Arc::clone(&hot);
                let cold = Arc::clone(&cold);
                let barrier = Arc::clone(&barrier);

                scope.spawn(move || {
                    let mut position = worker * 997;
                    barrier.wait();

                    for fresh in &cold[worker] {
                        if matches!(workload, Workload::Mixed) {
                            for _ in 0..49 {
                                position = (position + 7919) % hot.len();
                                black_box(index.encode(&hot[position]));
                            }
                        }

                        black_box(index.encode(fresh));
                    }
                })
            })
            .collect::<Vec<_>>();

        barrier.wait();
        let started = Instant::now();

        for handle in handles {
            handle.join().unwrap();
        }

        started.elapsed()
    });

    let writes = if matches!(workload, Workload::Mixed) {
        8 * COLD_PER_WRITER * 50
    } else {
        8 * COLD_PER_WRITER
    };

    let writes_s = writes as f64 / elapsed.as_secs_f64();

    Rates {
        writes_s,
        reads_s: 0.0,
        total_s: writes_s,
    }
}

fn run<T: BenchIndex>(workload: Workload) -> Rates {
    match workload {
        Workload::Mixed | Workload::Cold => run_bounded::<T>(workload),
        _ => run_timed::<T>(workload),
    }
}

fn run_target(target: Target, workload: Workload) -> Rates {
    match target {
        Target::Index => run::<Index>(workload),
        Target::Simple => run::<Simple>(workload),
        Target::V10A => run::<V10A>(workload),
        Target::V10APacked => run::<V10APacked>(workload),
        Target::V11A => run::<V11A>(workload),
        Target::V11APacked => run::<V11APacked>(workload),
    }
}

fn summary(mut values: Vec<f64>) -> (f64, f64, f64) {
    values.sort_by(f64::total_cmp);
    (
        values[0],
        values[values.len() / 2],
        values[values.len() - 1],
    )
}

#[test]
#[ignore = "fixed-topology representative index workload benchmark"]
fn bench_workloads() {
    for workload in Workload::ALL {
        let mut results = std::collections::BTreeMap::<&str, Vec<Rates>>::new();

        for repetition in 0..REPETITIONS {
            let mut targets = Target::WORKLOAD;
            let target_count = targets.len();
            targets.rotate_left(repetition % target_count);

            if repetition.is_multiple_of(2) {
                targets.reverse();
            }

            for target in targets {
                if selected(target) {
                    continue;
                }

                let rates = run_target(target, workload);
                println!(
                    "workload={} target={} repetition={} write_ops_s={:.0} read_ops_s={:.0} total_ops_s={:.0}",
                    workload.name(),
                    target.name(),
                    repetition + 1,
                    rates.writes_s,
                    rates.reads_s,
                    rates.total_s,
                );

                results.entry(target.name()).or_default().push(rates);
            }
        }

        for (target, rates) in results {
            let writes = summary(rates.iter().map(|rate| rate.writes_s).collect());
            let reads = summary(rates.iter().map(|rate| rate.reads_s).collect());
            let total = summary(rates.iter().map(|rate| rate.total_s).collect());

            println!(
                "workload={} target={target}                  write_median_ops_s={:.0} write_range=[{:.0}..{:.0}]                  read_median_ops_s={:.0} read_range=[{:.0}..{:.0}]                  total_median_ops_s={:.0} total_range=[{:.0}..{:.0}]",
                workload.name(),
                writes.1,
                writes.0,
                writes.2,
                reads.1,
                reads.0,
                reads.2,
                total.1,
                total.0,
                total.2,
            );
        }
    }
}
