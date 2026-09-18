use std::hint::black_box;
use std::time::{Duration, Instant};

use super::shared::{Shape, build_query_bank, seeded_series_labels};
use super::targets::{BenchIndex, Index, Simple, Target, V10A, V10APacked, V11A, V11APacked};

const SEED_SERIES: usize = 100_000;
const REPETITIONS: usize = 5;
const WARMUP: Duration = Duration::from_secs(1);
const MEASURE: Duration = Duration::from_secs(3);
const CARDINALITY_SAMPLE: usize = 128;

#[derive(Debug, Clone, Copy)]
enum Mode {
    Resolve,
    ResolveInspect,
}

impl Mode {
    const ALL: [Self; 2] = [Self::Resolve, Self::ResolveInspect];

    fn name(self) -> &'static str {
        match self {
            Self::Resolve => "resolve",
            Self::ResolveInspect => "resolve_inspect",
        }
    }
}

fn selected(target: Target) -> bool {
    std::env::var("TSDB_BENCH_TARGET").is_ok_and(|value| value != target.name())
}

fn selected_shape(shape: Shape) -> bool {
    std::env::var("TSDB_QUERY_SHAPE").is_ok_and(|value| value != shape.name())
}

fn seed<T: BenchIndex>(index: &T) {
    for raw_id in 0..SEED_SERIES as u64 {
        black_box(index.encode(&T::native(seeded_series_labels(raw_id))));
    }
}

fn run_for<T: BenchIndex>(
    index: &T,
    queries: &[Vec<tsdb::model::Matcher>],
    mode: Mode,
    duration: Duration,
) -> f64 {
    let started = Instant::now();
    let mut operations = 0_u64;

    loop {
        for query in queries {
            let ids = index.resolve(query);
            match mode {
                Mode::Resolve => {
                    black_box(ids);
                }
                Mode::ResolveInspect => {
                    let mut checksum = 0usize;
                    for id in ids {
                        checksum = checksum.wrapping_add(index.inspect_labels(id).unwrap_or(0));
                    }
                    black_box(checksum);
                }
            }
            operations += 1;
        }

        if started.elapsed() >= duration {
            break;
        }
    }

    operations as f64 / started.elapsed().as_secs_f64()
}

fn run<T: BenchIndex>(shape: Shape, mode: Mode) -> f64 {
    let index = T::new();
    seed(&index);
    let queries = build_query_bank(shape, SEED_SERIES);

    black_box(run_for(&index, &queries, mode, WARMUP));
    run_for(&index, &queries, mode, MEASURE)
}

fn run_target(target: Target, shape: Shape, mode: Mode) -> f64 {
    match target {
        Target::Index => run::<Index>(shape, mode),
        Target::Simple => run::<Simple>(shape, mode),
        Target::V10A => run::<V10A>(shape, mode),
        Target::V10APacked => run::<V10APacked>(shape, mode),
        Target::V11A => run::<V11A>(shape, mode),
        Target::V11APacked => run::<V11APacked>(shape, mode),
    }
}

fn cardinality_summary(shape: Shape) -> (usize, usize, usize) {
    let index = Simple::new();
    seed(&index);
    let queries = build_query_bank(shape, SEED_SERIES);

    let mut sizes = queries
        .iter()
        .take(CARDINALITY_SAMPLE)
        .map(|query| index.resolve(query).len())
        .collect::<Vec<_>>();

    sizes.sort_unstable();
    (sizes[0], sizes[sizes.len() / 2], sizes[sizes.len() - 1])
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
#[ignore = "single-thread query-shape benchmark"]
fn bench_query_shapes() {
    for shape in Shape::ALL {
        if selected_shape(shape) {
            continue;
        }

        let cardinality = cardinality_summary(shape);
        println!(
            "query_shape={} cardinality_sample={} result_min={} result_median={} result_max={}",
            shape.name(),
            CARDINALITY_SAMPLE,
            cardinality.0,
            cardinality.1,
            cardinality.2,
        );

        for mode in Mode::ALL {
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
                    let queries_s = run_target(target, shape, mode);
                    println!(
                        "query_shape={} mode={} target={} repetition={} queries_s={queries_s:.0}",
                        shape.name(),
                        mode.name(),
                        target.name(),
                        repetition + 1,
                    );
                    results.entry(target.name()).or_default().push(queries_s);
                }
            }

            for (target, values) in results {
                let (low, median, high) = summary(values);
                println!(
                    "query_shape={} mode={} target={target} median_queries_s={median:.0} range=[{low:.0}..{high:.0}]",
                    shape.name(),
                    mode.name(),
                );
            }
        }
    }
}
