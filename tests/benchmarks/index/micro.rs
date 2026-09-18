use std::hint::black_box;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use tsdb::model::SeriesId;

use super::shared::{Shape, build_query_bank, cold_series_labels, seeded_series_labels};
use super::targets::{BenchIndex, Index, Simple, Target, V10A, V10APacked, V11A, V11APacked};

const THREADS: usize = 8;
const SEED_SERIES: usize = 100_000;
const REPETITIONS: usize = 5;
const WARMUP: Duration = Duration::from_secs(1);
const MEASURE: Duration = Duration::from_secs(3);
const COLD_PER_THREAD: usize = 40_000;

#[derive(Debug, Clone, Copy)]
enum Operation {
    HotEncode,
    Lookup,
    Forward,
    Resolve,
    ResolveInspect,
    ColdCreate,
}

impl Operation {
    const ALL: [Self; 6] = [
        Self::HotEncode,
        Self::Lookup,
        Self::Forward,
        Self::Resolve,
        Self::ResolveInspect,
        Self::ColdCreate,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::HotEncode => "hot_encode_8w",
            Self::Lookup => "lookup_8r",
            Self::Forward => "forward_labels_8r",
            Self::Resolve => "resolve_8q",
            Self::ResolveInspect => "resolve_inspect_8q",
            Self::ColdCreate => "cold_create_8w",
        }
    }
}

fn selected(target: Target) -> bool {
    std::env::var("TSDB_BENCH_TARGET").is_ok_and(|value| value != target.name())
}

fn selected_operation(operation: Operation) -> bool {
    std::env::var("TSDB_MICRO_OPERATION").is_ok_and(|value| value != operation.name())
}

struct Prepared<T: BenchIndex> {
    index: Arc<T>,
    labels: Arc<Vec<T::Labels>>,
    ids: Arc<Vec<SeriesId>>,
}

fn prepared<T: BenchIndex>() -> Prepared<T> {
    let labels = Arc::new(
        (0..SEED_SERIES as u64)
            .map(seeded_series_labels)
            .map(T::native)
            .collect::<Vec<_>>(),
    );
    let index = Arc::new(T::new());
    let ids = Arc::new(labels.iter().map(|labels| index.encode(labels)).collect());

    Prepared { index, labels, ids }
}

fn timed_phase<T: BenchIndex>(
    index: &Arc<T>,
    labels: &Arc<Vec<T::Labels>>,
    ids: &Arc<Vec<SeriesId>>,
    queries: &Arc<Vec<Vec<tsdb::model::Matcher>>>,
    operation: Operation,
    duration: Duration,
) -> f64 {
    let stop = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(Barrier::new(THREADS + 1));

    let (operations, elapsed) = std::thread::scope(|scope| {
        let handles = (0..THREADS)
            .map(|thread| {
                let index = Arc::clone(index);
                let labels = Arc::clone(labels);
                let ids = Arc::clone(ids);
                let queries = Arc::clone(queries);
                let stop = Arc::clone(&stop);
                let barrier = Arc::clone(&barrier);

                scope.spawn(move || {
                    let mut operations = 0_u64;
                    let mut position = thread * 997;
                    barrier.wait();

                    match operation {
                        Operation::HotEncode => {
                            while !stop.load(Ordering::Relaxed) {
                                position = (position + 7919) % labels.len();
                                black_box(index.encode(&labels[position]));
                                operations += 1;
                            }
                        }
                        Operation::Lookup => {
                            while !stop.load(Ordering::Relaxed) {
                                position = (position + 7919) % labels.len();
                                black_box(index.lookup(&labels[position]));
                                operations += 1;
                            }
                        }
                        Operation::Forward => {
                            while !stop.load(Ordering::Relaxed) {
                                position = (position + 7919) % ids.len();
                                black_box(index.inspect_labels(ids[position]));
                                operations += 1;
                            }
                        }
                        Operation::Resolve => {
                            while !stop.load(Ordering::Relaxed) {
                                position = (position + 17) % queries.len();
                                black_box(index.resolve(&queries[position]));
                                operations += 1;
                            }
                        }
                        Operation::ResolveInspect => {
                            while !stop.load(Ordering::Relaxed) {
                                position = (position + 17) % queries.len();
                                let resolved = index.resolve(&queries[position]);
                                let mut checksum = 0usize;
                                for id in resolved {
                                    checksum = checksum
                                        .wrapping_add(index.inspect_labels(id).unwrap_or(0));
                                }
                                black_box(checksum);
                                operations += 1;
                            }
                        }
                        Operation::ColdCreate => unreachable!(),
                    }

                    operations
                })
            })
            .collect::<Vec<_>>();

        barrier.wait();
        let started = Instant::now();
        std::thread::sleep(duration);
        stop.store(true, Ordering::Relaxed);
        let elapsed = started.elapsed();

        let operations = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .sum::<u64>();

        (operations, elapsed)
    });

    operations as f64 / elapsed.as_secs_f64()
}

fn cold<T: BenchIndex>(repetition: usize) -> f64 {
    let offset = (repetition as u64 + 1) * 100_000_000;
    let labels = Arc::new(
        (0..THREADS)
            .map(|thread| {
                (0..COLD_PER_THREAD as u64)
                    .map(|n| {
                        T::native(cold_series_labels(
                            thread,
                            offset + (thread * COLD_PER_THREAD) as u64 + n,
                        ))
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>(),
    );

    let index = Arc::new(T::new());
    for raw_id in 0..SEED_SERIES as u64 {
        let seeded = T::native(seeded_series_labels(raw_id));
        black_box(index.encode(&seeded));
    }
    assert!(
        labels
            .iter()
            .flatten()
            .all(|labels| index.lookup(labels).is_none())
    );

    let barrier = Arc::new(Barrier::new(THREADS + 1));

    let elapsed = std::thread::scope(|scope| {
        let handles = (0..THREADS)
            .map(|thread| {
                let index = Arc::clone(&index);
                let labels = Arc::clone(&labels);
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    barrier.wait();
                    for labels in &labels[thread] {
                        black_box(index.encode(labels));
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

    (THREADS * COLD_PER_THREAD) as f64 / elapsed.as_secs_f64()
}

fn run<T: BenchIndex>(operation: Operation, repetition: usize) -> f64 {
    match operation {
        Operation::ColdCreate => cold::<T>(repetition),
        _ => {
            let Prepared { index, labels, ids } = prepared::<T>();
            let queries = build_query_bank(Shape::Conj, SEED_SERIES);
            black_box(timed_phase(
                &index, &labels, &ids, &queries, operation, WARMUP,
            ));
            timed_phase(&index, &labels, &ids, &queries, operation, MEASURE)
        }
    }
}

fn run_target(target: Target, operation: Operation, repetition: usize) -> f64 {
    match target {
        Target::Index => run::<Index>(operation, repetition),
        Target::Simple => run::<Simple>(operation, repetition),
        Target::V10A => run::<V10A>(operation, repetition),
        Target::V10APacked => run::<V10APacked>(operation, repetition),
        Target::V11A => run::<V11A>(operation, repetition),
        Target::V11APacked => run::<V11APacked>(operation, repetition),
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
#[ignore = "fixed-8-worker primitive index microbenchmark"]
fn bench_index_micro() {
    for operation in Operation::ALL {
        if selected_operation(operation) {
            continue;
        }

        let mut results = std::collections::BTreeMap::<&str, Vec<f64>>::new();

        for repetition in 0..REPETITIONS {
            let mut targets = Target::PERFORMANCE;
            let target_count = targets.len();
            targets.rotate_left(repetition % target_count);
            if repetition.is_multiple_of(2) {
                targets.reverse();
            }

            for target in targets {
                if selected(target) {
                    continue;
                }
                let ops_s = run_target(target, operation, repetition);
                println!(
                    "micro operation={} target={} repetition={} ops_s={ops_s:.0}",
                    operation.name(),
                    target.name(),
                    repetition + 1,
                );
                results.entry(target.name()).or_default().push(ops_s);
            }
        }

        for (target, values) in results {
            let (low, median, high) = summary(values);
            println!(
                "micro operation={} target={target} median_ops_s={median:.0} range=[{low:.0}..{high:.0}]",
                operation.name(),
            );
        }
    }
}
