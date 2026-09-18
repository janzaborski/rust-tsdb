use std::hint::black_box;
use std::time::{Duration, Instant};

use tsdb::model::SeriesId;

use super::shared::{Shape, build_query_bank, cold_series_labels, seeded_series_labels};
use super::targets::{BenchIndex, Index, Simple, Target, V10A, V10APacked, V11A, V11APacked};

const SEED_SERIES: usize = 100_000;
const REPETITIONS: usize = 5;
const WARMUP: Duration = Duration::from_millis(500);
const MEASURE: Duration = Duration::from_secs(2);
const COLD_SERIES: usize = 100_000;

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
            Self::HotEncode => "hot_encode",
            Self::Lookup => "lookup",
            Self::Forward => "forward_labels",
            Self::Resolve => "resolve",
            Self::ResolveInspect => "resolve_inspect",
            Self::ColdCreate => "cold_create",
        }
    }
}

fn selected(target: Target) -> bool {
    std::env::var("TSDB_BENCH_TARGET").is_ok_and(|value| value != target.name())
}

fn selected_operation(operation: Operation) -> bool {
    std::env::var("TSDB_CORE_OPERATION").is_ok_and(|value| value != operation.name())
}

fn run_for(mut function: impl FnMut(), duration: Duration) -> f64 {
    let started = Instant::now();
    let mut operations = 0_u64;

    loop {
        for _ in 0..1024 {
            function();
            operations += 1;
        }
        if started.elapsed() >= duration {
            break;
        }
    }

    operations as f64 / started.elapsed().as_secs_f64()
}

fn seeded<T: BenchIndex>() -> (T, Vec<T::Labels>, Vec<SeriesId>) {
    let labels = (0..SEED_SERIES as u64)
        .map(seeded_series_labels)
        .map(T::native)
        .collect::<Vec<_>>();

    let index = T::new();
    let ids = labels
        .iter()
        .map(|labels| index.encode(labels))
        .collect::<Vec<_>>();

    (index, labels, ids)
}

fn hot_encode<T: BenchIndex>() -> f64 {
    let (index, labels, _) = seeded::<T>();
    let mut position = 0usize;

    black_box(run_for(
        || {
            position = (position + 7919) % labels.len();
            black_box(index.encode(&labels[position]));
        },
        WARMUP,
    ));

    run_for(
        || {
            position = (position + 7919) % labels.len();
            black_box(index.encode(&labels[position]));
        },
        MEASURE,
    )
}

fn lookup<T: BenchIndex>() -> f64 {
    let (index, labels, _) = seeded::<T>();
    let mut position = 0usize;

    black_box(run_for(
        || {
            position = (position + 7919) % labels.len();
            black_box(index.lookup(&labels[position]));
        },
        WARMUP,
    ));

    run_for(
        || {
            position = (position + 7919) % labels.len();
            black_box(index.lookup(&labels[position]));
        },
        MEASURE,
    )
}

fn forward<T: BenchIndex>() -> f64 {
    let (index, _, ids) = seeded::<T>();
    let mut position = 0usize;

    black_box(run_for(
        || {
            position = (position + 7919) % ids.len();
            black_box(index.inspect_labels(ids[position]));
        },
        WARMUP,
    ));

    run_for(
        || {
            position = (position + 7919) % ids.len();
            black_box(index.inspect_labels(ids[position]));
        },
        MEASURE,
    )
}

fn resolve<T: BenchIndex>(inspect: bool) -> f64 {
    let (index, _, _) = seeded::<T>();
    let queries = build_query_bank(Shape::Conj, SEED_SERIES);
    let mut position = 0usize;

    let mut execute = || {
        position = (position + 17) % queries.len();
        let ids = index.resolve(&queries[position]);
        if inspect {
            let mut checksum = 0usize;
            for id in ids {
                checksum = checksum.wrapping_add(index.inspect_labels(id).unwrap_or(0));
            }
            black_box(checksum);
        } else {
            black_box(ids);
        }
    };

    black_box(run_for(&mut execute, WARMUP));
    run_for(&mut execute, MEASURE)
}

fn cold_create<T: BenchIndex>(repetition: usize) -> f64 {
    let index = T::new();
    for raw_id in 0..SEED_SERIES as u64 {
        let labels = T::native(seeded_series_labels(raw_id));
        black_box(index.encode(&labels));
    }

    let offset = (repetition as u64 + 1) * 10_000_000;
    let labels = (0..COLD_SERIES as u64)
        .map(|n| T::native(cold_series_labels(0, offset + n)))
        .collect::<Vec<_>>();

    assert!(labels.iter().all(|labels| index.lookup(labels).is_none()));

    let started = Instant::now();
    for labels in &labels {
        black_box(index.encode(labels));
    }
    COLD_SERIES as f64 / started.elapsed().as_secs_f64()
}

fn run<T: BenchIndex>(operation: Operation, repetition: usize) -> f64 {
    match operation {
        Operation::HotEncode => hot_encode::<T>(),
        Operation::Lookup => lookup::<T>(),
        Operation::Forward => forward::<T>(),
        Operation::Resolve => resolve::<T>(false),
        Operation::ResolveInspect => resolve::<T>(true),
        Operation::ColdCreate => cold_create::<T>(repetition),
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
#[ignore = "single-thread primitive index benchmark"]
fn bench_index_core() {
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
                    "core operation={} target={} repetition={} ops_s={ops_s:.0}",
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
                "core operation={} target={target} median_ops_s={median:.0} range=[{low:.0}..{high:.0}]",
                operation.name(),
            );
        }
    }
}
