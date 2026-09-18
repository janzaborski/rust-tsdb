use std::hint::black_box;
use std::time::{Duration, Instant};

use super::shared::{cold_series_labels, seeded_series_labels};
use super::targets::{BenchIndex, Index, Simple, Target, V10A, V10APacked, V11A, V11APacked};

const SEED_SERIES: usize = 100_000;
const SAMPLES: usize = 50_000;
const REPETITIONS: usize = 5;

#[derive(Debug, Clone, Copy)]
enum Operation {
    HotEncode,
    ColdCreate,
}

impl Operation {
    const ALL: [Self; 2] = [Self::HotEncode, Self::ColdCreate];

    fn name(self) -> &'static str {
        match self {
            Self::HotEncode => "hot_encode",
            Self::ColdCreate => "cold_create",
        }
    }
}

#[derive(Debug)]
struct LatencySummary {
    min: Duration,
    p50: Duration,
    p95: Duration,
    p99: Duration,
    p999: Duration,
    max: Duration,
    mean_ns: f64,
}

fn selected(target: Target) -> bool {
    std::env::var("TSDB_BENCH_TARGET").is_ok_and(|value| value != target.name())
}

fn selected_operation(operation: Operation) -> bool {
    std::env::var("TSDB_LATENCY_OPERATION").is_ok_and(|value| value != operation.name())
}

fn seed<T: BenchIndex>(index: &T) {
    for raw_id in 0..SEED_SERIES as u64 {
        let labels = T::native(seeded_series_labels(raw_id));
        black_box(index.encode(black_box(&labels)));
    }
}

#[inline]
fn percentile(sorted: &[Duration], numerator: usize, denominator: usize) -> Duration {
    assert!(!sorted.is_empty());
    assert!(numerator <= denominator);

    let index = (sorted.len() - 1) * numerator / denominator;
    sorted[index]
}

fn summarize(mut samples: Vec<Duration>) -> LatencySummary {
    assert!(!samples.is_empty());

    samples.sort_unstable();

    let total_ns = samples.iter().map(Duration::as_nanos).sum::<u128>();

    LatencySummary {
        min: samples[0],
        p50: percentile(&samples, 50, 100),
        p95: percentile(&samples, 95, 100),
        p99: percentile(&samples, 99, 100),
        p999: percentile(&samples, 999, 1000),
        max: samples[samples.len() - 1],
        mean_ns: total_ns as f64 / samples.len() as f64,
    }
}

#[inline]
fn nanos(duration: Duration) -> u128 {
    duration.as_nanos()
}

fn hot_encode<T: BenchIndex>(repetition: usize) -> Vec<Duration> {
    let labels = (0..SEED_SERIES as u64)
        .map(seeded_series_labels)
        .map(T::native)
        .collect::<Vec<_>>();

    let index = T::new();

    for labels in &labels {
        black_box(index.encode(black_box(labels)));
    }

    let mut samples = Vec::with_capacity(SAMPLES);
    let mut position = repetition * 997 % labels.len();

    for _ in 0..SAMPLES {
        position = (position + 7919) % labels.len();

        let labels = black_box(&labels[position]);

        let started = Instant::now();
        black_box(index.encode(labels));
        samples.push(started.elapsed());
    }

    samples
}

fn cold_create<T: BenchIndex>(repetition: usize) -> Vec<Duration> {
    let index = T::new();
    seed(&index);

    let offset = 10_000_000_u64 + repetition as u64 * SAMPLES as u64;

    let labels = (0..SAMPLES as u64)
        .map(|n| T::native(cold_series_labels(0, offset + n)))
        .collect::<Vec<_>>();

    assert!(labels.iter().all(|labels| index.lookup(labels).is_none()));

    let mut samples = Vec::with_capacity(SAMPLES);

    for labels in &labels {
        let labels = black_box(labels);

        let started = Instant::now();
        black_box(index.encode(labels));
        samples.push(started.elapsed());
    }

    samples
}

fn run<T: BenchIndex>(operation: Operation, repetition: usize) -> Vec<Duration> {
    match operation {
        Operation::HotEncode => hot_encode::<T>(repetition),
        Operation::ColdCreate => cold_create::<T>(repetition),
    }
}

fn run_target(target: Target, operation: Operation, repetition: usize) -> Vec<Duration> {
    match target {
        Target::Index => run::<Index>(operation, repetition),
        Target::Simple => run::<Simple>(operation, repetition),
        Target::V10A => run::<V10A>(operation, repetition),
        Target::V10APacked => run::<V10APacked>(operation, repetition),
        Target::V11A => run::<V11A>(operation, repetition),
        Target::V11APacked => run::<V11APacked>(operation, repetition),
    }
}

#[test]
#[ignore = "single-thread primitive index write-latency benchmark"]
fn bench_index_latency() {
    for operation in Operation::ALL {
        if selected_operation(operation) {
            continue;
        }

        let mut results = std::collections::BTreeMap::<&str, Vec<Duration>>::new();

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

                let samples = run_target(target, operation, repetition);

                let summary = summarize(samples.clone());

                println!(
                    concat!(
                        "latency operation={} target={} repetition={} ",
                        "samples={} ",
                        "min_ns={} ",
                        "p50_ns={} ",
                        "p95_ns={} ",
                        "p99_ns={} ",
                        "p999_ns={} ",
                        "max_ns={} ",
                        "mean_ns={:.2}"
                    ),
                    operation.name(),
                    target.name(),
                    repetition + 1,
                    samples.len(),
                    nanos(summary.min),
                    nanos(summary.p50),
                    nanos(summary.p95),
                    nanos(summary.p99),
                    nanos(summary.p999),
                    nanos(summary.max),
                    summary.mean_ns,
                );

                results.entry(target.name()).or_default().extend(samples);
            }
        }

        for (target, samples) in results {
            let count = samples.len();
            let summary = summarize(samples);

            println!(
                concat!(
                    "latency_summary operation={} target={} ",
                    "samples={} ",
                    "min_ns={} ",
                    "p50_ns={} ",
                    "p95_ns={} ",
                    "p99_ns={} ",
                    "p999_ns={} ",
                    "max_ns={} ",
                    "mean_ns={:.2}"
                ),
                operation.name(),
                target,
                count,
                nanos(summary.min),
                nanos(summary.p50),
                nanos(summary.p95),
                nanos(summary.p99),
                nanos(summary.p999),
                nanos(summary.max),
                summary.mean_ns,
            );
        }
    }
}
