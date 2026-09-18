use std::hint::black_box;
use std::mem::{size_of, size_of_val};
use std::time::{Duration, Instant};

use tsdb::utils::algorithms::set_ops::*;

const RUNS_PER_CASE: usize = 5;
const TARGET_BATCH_BYTES: usize = 4 * 1024 * 1024;
const MIN_BATCH: usize = 8;
const MAX_BATCH: usize = 4096;
const MAX_POSTING_LEN: usize = 262_144;

const SMALLER_SIZES: &[usize] = &[16, 64, 256, 1_024];
const RATIOS: &[usize] = &[1, 8, 16, 24, 32, 64, 128];

const NEAR_BEST_RATIO: f64 = 1.05;

#[derive(Debug, Clone, Copy)]
enum Pattern {
    InterleavedZero,
    Overlap10,
    Overlap50,
    Subset,
}

impl Pattern {
    const ALL: [Self; 4] = [
        Self::InterleavedZero,
        Self::Overlap10,
        Self::Overlap50,
        Self::Subset,
    ];

    fn overlap_percent(self) -> usize {
        match self {
            Self::InterleavedZero => 0,
            Self::Overlap10 => 10,
            Self::Overlap50 => 50,
            Self::Subset => 100,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TimingStats {
    median: Duration,
}

#[derive(Clone, Copy)]
struct Candidate<T> {
    name: &'static str,
    run: T,
}

#[derive(Debug, Clone)]
struct Aggregate {
    name: &'static str,
    cases: usize,
    sum_ns_per_input: f64,
    total_ns: f64,
    total_best_ns: f64,
    total_input: usize,
    sum_log_vs_best: f64,
    within_5pct: usize,
}

impl Aggregate {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            cases: 0,
            sum_ns_per_input: 0.0,
            total_ns: 0.0,
            total_best_ns: 0.0,
            total_input: 0,
            sum_log_vs_best: 0.0,
            within_5pct: 0,
        }
    }

    fn record(&mut self, elapsed_ns: f64, input_ids: usize, best_ns: f64) {
        debug_assert!(input_ids > 0);
        debug_assert!(elapsed_ns > 0.0);
        debug_assert!(best_ns > 0.0);

        let relative = elapsed_ns / best_ns;

        self.cases += 1;
        self.sum_ns_per_input += elapsed_ns / input_ids as f64;
        self.total_ns += elapsed_ns;
        self.total_best_ns += best_ns;
        self.total_input += input_ids;
        self.sum_log_vs_best += relative.ln();

        if relative <= NEAR_BEST_RATIO {
            self.within_5pct += 1;
        }
    }

    fn mean_ns_per_input(&self) -> f64 {
        self.sum_ns_per_input / self.cases as f64
    }

    fn weighted_ns_per_input(&self) -> f64 {
        self.total_ns / self.total_input as f64
    }

    fn regret_ns_per_input(&self) -> f64 {
        (self.total_ns - self.total_best_ns) / self.total_input as f64
    }

    fn geo_vs_best(&self) -> f64 {
        (self.sum_log_vs_best / self.cases as f64).exp()
    }

    fn near_best_percent(&self) -> f64 {
        self.within_5pct as f64 / self.cases as f64 * 100.0
    }
}

fn print_ranking(title: &str, mut results: Vec<Aggregate>) {
    results.sort_by(|a, b| a.mean_ns_per_input().total_cmp(&b.mean_ns_per_input()));

    println!("\n=== {title} ===");
    println!(
        "{:<4} {:<14} {:>16} {:>19} {:>18} {:>13} {:>13}",
        "rank",
        "algorithm",
        "mean ns/input",
        "weighted ns/input",
        "regret ns/input",
        "geo vs best",
        "within 5%"
    );

    for (rank, result) in results.iter().enumerate() {
        println!(
            "{:<4} {:<14} {:>16.4} {:>19.4} {:>18.4} {:>12.3}x {:>12.1}%",
            rank + 1,
            result.name,
            result.mean_ns_per_input(),
            result.weighted_ns_per_input(),
            result.regret_ns_per_input(),
            result.geo_vs_best(),
            result.near_best_percent(),
        );
    }
}

fn timing_stats(mut values: Vec<Duration>) -> TimingStats {
    values.sort_unstable();
    TimingStats {
        median: values[values.len() / 2],
    }
}

fn ns(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e9
}

fn batch_size(input_bytes: usize) -> usize {
    let input_bytes = input_bytes.max(size_of::<u64>());
    (TARGET_BATCH_BYTES / input_bytes).clamp(MIN_BATCH, MAX_BATCH)
}

fn strictly_increasing(values: &[u64]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

fn build_pair(a_len: usize, b_len: usize, pattern: Pattern) -> (Vec<u64>, Vec<u64>) {
    if matches!(pattern, Pattern::InterleavedZero) {
        let a = (0..a_len as u64).map(|i| i * 4).collect::<Vec<_>>();
        let b = (0..b_len as u64).map(|i| i * 4 + 2).collect::<Vec<_>>();
        return (a, b);
    }

    let smaller = a_len.min(b_len);
    let shared = (smaller * pattern.overlap_percent() / 100)
        .max(1)
        .min(smaller);

    let a_unique = a_len - shared;
    let b_unique = b_len - shared;
    let span = a_len.max(b_len).max(1) as u64 * 8 + 1;

    let spread = |index: usize, count: usize, tag: u64| -> u64 {
        let bucket = if count == 0 {
            0
        } else {
            ((index as u128 + 1) * span as u128 / (count as u128 + 1)) as u64
        };
        bucket * 4 + tag
    };

    let shared_values = (0..shared)
        .map(|i| spread(i, shared, 0))
        .collect::<Vec<_>>();

    let mut a = Vec::with_capacity(a_len);
    a.extend(shared_values.iter().copied());
    a.extend((0..a_unique).map(|i| spread(i, a_unique, 1)));
    a.sort_unstable();

    let mut b = Vec::with_capacity(b_len);
    b.extend(shared_values);
    b.extend((0..b_unique).map(|i| spread(i, b_unique, 2)));
    b.sort_unstable();

    assert!(strictly_increasing(&a));
    assert!(strictly_increasing(&b));
    (a, b)
}

fn primitive_intersect_linear(a: &mut Vec<u64>, b: &[u64]) {
    let a_end = a.len();
    intersect_linear_range(a, 0, a_end, b, 0, b.len());
}

fn primitive_intersect_binary(a: &mut Vec<u64>, b: &[u64]) {
    let a_end = a.len();
    intersect_binary_range(a, a_end, b, 0, b.len());
}

fn primitive_intersect_gallop(a: &mut Vec<u64>, b: &[u64]) {
    let a_end = a.len();
    intersect_gallop_range(a, a_end, b, 0, b.len());
}

fn primitive_union_linear(a: &[u64], b: &[u64]) -> Vec<u64> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    union_linear_into(a, b, &mut out);
    out
}

fn primitive_union_gallop(a: &[u64], b: &[u64]) -> Vec<u64> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    union_gallop_runs_into(a, b, &mut out);
    out
}

fn primitive_subtract_linear(a: &mut Vec<u64>, b: &[u64]) {
    let original_len = a.len();
    let write_end = subtract_linear_range(a, 0, original_len, b, 0, b.len());
    a.truncate(write_end);
}

fn primitive_subtract_binary(a: &mut Vec<u64>, b: &[u64]) {
    let original_len = a.len();
    let write_end = subtract_binary_range(a, original_len, b, 0, b.len());
    a.truncate(write_end);
}

fn primitive_subtract_seek(a: &mut Vec<u64>, b: &[u64]) {
    let original_len = a.len();
    let write_end = subtract_seek_range(a, original_len, b, 0, b.len());
    a.truncate(write_end);
}

fn primitive_subtract_sparse(a: &mut Vec<u64>, b: &[u64]) {
    let original_len = a.len();
    let write_end = subtract_sparse_range(a, 0, original_len, b);
    a.truncate(write_end);
}

fn primitive_many_pairwise(lists: &[&[u64]]) -> Vec<u64> {
    let total = lists.iter().map(|posting| posting.len()).sum::<usize>();
    union_many_pairwise_reuse(lists, total)
}

fn primitive_many_concat_sort(lists: &[&[u64]]) -> Vec<u64> {
    let total = lists.iter().map(|posting| posting.len()).sum::<usize>();
    union_many_disjoint_concat_sort(lists, total)
}

type InPlaceOp = fn(&mut Vec<u64>, &[u64]);
type UnionOp = fn(&[u64], &[u64]) -> Vec<u64>;
type ManyOp = fn(&[&[u64]]) -> Vec<u64>;

fn measure_in_place(run: InPlaceOp, a: &[u64], b: &[u64]) -> TimingStats {
    let batch = batch_size(size_of_val(a));
    let mut runs = Vec::with_capacity(RUNS_PER_CASE);

    for _ in 0..RUNS_PER_CASE {
        let mut inputs = (0..batch).map(|_| a.to_vec()).collect::<Vec<_>>();
        black_box(&mut inputs);

        let started = Instant::now();
        let mut checksum = 0usize;

        for input in &mut inputs {
            run(black_box(input), black_box(b));
            checksum = checksum.wrapping_add(input.len());
            black_box(&*input);
        }

        black_box(checksum);
        runs.push(started.elapsed().div_f64(batch as f64));
    }

    timing_stats(runs)
}

fn measure_union(run: UnionOp, a: &[u64], b: &[u64]) -> TimingStats {
    let batch = batch_size(size_of_val(a) + size_of_val(b));
    let mut runs = Vec::with_capacity(RUNS_PER_CASE);

    for _ in 0..RUNS_PER_CASE {
        let started = Instant::now();
        let mut checksum = 0usize;

        for _ in 0..batch {
            let out = run(black_box(a), black_box(b));
            checksum = checksum.wrapping_add(out.len());
            black_box(out);
        }

        black_box(checksum);
        runs.push(started.elapsed().div_f64(batch as f64));
    }

    timing_stats(runs)
}

fn measure_many(run: ManyOp, lists: &[&[u64]]) -> TimingStats {
    let total = lists.iter().map(|posting| posting.len()).sum::<usize>();
    let batch = batch_size(total.saturating_mul(size_of::<u64>()));
    let mut runs = Vec::with_capacity(RUNS_PER_CASE);

    for _ in 0..RUNS_PER_CASE {
        let started = Instant::now();
        let mut checksum = 0usize;

        for _ in 0..batch {
            let out = run(black_box(lists));
            checksum = checksum.wrapping_add(out.len());
            black_box(out);
        }

        black_box(checksum);
        runs.push(started.elapsed().div_f64(batch as f64));
    }

    timing_stats(runs)
}

fn record_case(aggregates: &mut [Aggregate], timings: &[TimingStats], input_ids: usize) {
    assert_eq!(aggregates.len(), timings.len());

    let best_ns = timings
        .iter()
        .map(|timing| ns(timing.median))
        .min_by(f64::total_cmp)
        .expect("at least one timing");

    for (aggregate, timing) in aggregates.iter_mut().zip(timings) {
        aggregate.record(ns(timing.median), input_ids, best_ns);
    }
}

fn rank_intersection() -> Vec<Aggregate> {
    let candidates: [Candidate<InPlaceOp>; 4] = [
        Candidate {
            name: "production",
            run: intersect_in_place,
        },
        Candidate {
            name: "linear",
            run: primitive_intersect_linear,
        },
        Candidate {
            name: "binary",
            run: primitive_intersect_binary,
        },
        Candidate {
            name: "gallop",
            run: primitive_intersect_gallop,
        },
    ];

    let mut aggregates = candidates
        .iter()
        .map(|candidate| Aggregate::new(candidate.name))
        .collect::<Vec<_>>();

    for &small in SMALLER_SIZES {
        for &ratio in RATIOS {
            let large = small.saturating_mul(ratio);
            if large > MAX_POSTING_LEN {
                continue;
            }

            for pattern in Pattern::ALL {
                let (a, b) = build_pair(small, large, pattern);

                let mut expected = a.clone();
                primitive_intersect_linear(&mut expected, &b);

                let timings = candidates
                    .iter()
                    .map(|candidate| {
                        let mut check = a.clone();
                        (candidate.run)(&mut check, &b);
                        assert_eq!(check, expected);
                        measure_in_place(candidate.run, &a, &b)
                    })
                    .collect::<Vec<_>>();

                record_case(&mut aggregates, &timings, a.len() + b.len());
            }
        }
    }

    aggregates
}

fn rank_union() -> Vec<Aggregate> {
    let candidates: [Candidate<UnionOp>; 3] = [
        Candidate {
            name: "production",
            run: union_sorted,
        },
        Candidate {
            name: "linear",
            run: primitive_union_linear,
        },
        Candidate {
            name: "gallop",
            run: primitive_union_gallop,
        },
    ];

    let mut aggregates = candidates
        .iter()
        .map(|candidate| Aggregate::new(candidate.name))
        .collect::<Vec<_>>();

    for &small in SMALLER_SIZES {
        for &ratio in RATIOS {
            let large = small.saturating_mul(ratio);
            if large > MAX_POSTING_LEN {
                continue;
            }

            for pattern in Pattern::ALL {
                let (a, b) = build_pair(small, large, pattern);
                let expected = primitive_union_linear(&a, &b);

                let timings = candidates
                    .iter()
                    .map(|candidate| {
                        let check = (candidate.run)(&a, &b);
                        assert_eq!(check, expected);
                        measure_union(candidate.run, &a, &b)
                    })
                    .collect::<Vec<_>>();

                record_case(&mut aggregates, &timings, a.len() + b.len());
            }
        }
    }

    aggregates
}

#[derive(Debug, Clone, Copy)]
enum Orientation {
    BLarger,
    ALarger,
}

impl Orientation {
    const ALL: [Self; 2] = [Self::BLarger, Self::ALarger];

    fn lengths(self, small: usize, large: usize) -> (usize, usize) {
        match self {
            Self::BLarger => (small, large),
            Self::ALarger => (large, small),
        }
    }
}

fn rank_subtraction() -> Vec<Aggregate> {
    let candidates: [Candidate<InPlaceOp>; 5] = [
        Candidate {
            name: "production",
            run: subtract_in_place,
        },
        Candidate {
            name: "linear",
            run: primitive_subtract_linear,
        },
        Candidate {
            name: "binary",
            run: primitive_subtract_binary,
        },
        Candidate {
            name: "seek",
            run: primitive_subtract_seek,
        },
        Candidate {
            name: "sparse",
            run: primitive_subtract_sparse,
        },
    ];

    let mut aggregates = candidates
        .iter()
        .map(|candidate| Aggregate::new(candidate.name))
        .collect::<Vec<_>>();

    for orientation in Orientation::ALL {
        for &small in SMALLER_SIZES {
            for &ratio in RATIOS {
                let large = small.saturating_mul(ratio);
                if large > MAX_POSTING_LEN {
                    continue;
                }

                let (a_len, b_len) = orientation.lengths(small, large);

                for pattern in Pattern::ALL {
                    let (a, b) = build_pair(a_len, b_len, pattern);

                    let mut expected = a.clone();
                    primitive_subtract_linear(&mut expected, &b);

                    let timings = candidates
                        .iter()
                        .map(|candidate| {
                            let mut check = a.clone();
                            (candidate.run)(&mut check, &b);
                            assert_eq!(check, expected);
                            measure_in_place(candidate.run, &a, &b)
                        })
                        .collect::<Vec<_>>();

                    record_case(&mut aggregates, &timings, a.len() + b.len());
                }
            }
        }
    }

    aggregates
}

fn build_disjoint_many(list_count: usize, per_list: usize) -> Vec<Vec<u64>> {
    (0..list_count)
        .map(|list| {
            (0..per_list)
                .map(|i| ((i * list_count + list) as u64) * 4 + 1)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn rank_multiway_union() -> Vec<Aggregate> {
    const LIST_COUNTS: &[usize] = &[2, 8, 16, 32, 64];
    const PER_LIST: &[usize] = &[64, 256, 1_024, 4_096];

    let candidates: [Candidate<ManyOp>; 3] = [
        Candidate {
            name: "production",
            run: union_many_disjoint,
        },
        Candidate {
            name: "pairwise",
            run: primitive_many_pairwise,
        },
        Candidate {
            name: "concat_sort",
            run: primitive_many_concat_sort,
        },
    ];

    let mut aggregates = candidates
        .iter()
        .map(|candidate| Aggregate::new(candidate.name))
        .collect::<Vec<_>>();

    for &list_count in LIST_COUNTS {
        for &per_list in PER_LIST {
            let total = list_count.saturating_mul(per_list);
            if total > MAX_POSTING_LEN {
                continue;
            }

            let owned = build_disjoint_many(list_count, per_list);
            let lists = owned.iter().map(Vec::as_slice).collect::<Vec<_>>();
            let expected = primitive_many_concat_sort(&lists);

            let timings = candidates
                .iter()
                .map(|candidate| {
                    let check = (candidate.run)(&lists);
                    assert_eq!(check, expected);
                    measure_many(candidate.run, &lists)
                })
                .collect::<Vec<_>>();

            record_case(&mut aggregates, &timings, total);
        }
    }

    aggregates
}

#[test]
#[ignore = "aggregate ranking of sorted-posting set-operation algorithms"]
fn bench_algorithm_rankings() {
    println!(
        "\nRanking metrics:\n\
         - mean ns/input: arithmetic mean of median_ns / total_input_ids per case (primary)\n\
         - weighted ns/input: total median ns / total input IDs across the matrix\n\
         - regret ns/input: total excess median ns over the per-case oracle / total input IDs\n\
         - geo vs best: geometric mean slowdown vs fastest candidate in each case (1.00x ideal)\n\
         - within 5%: share of cases within 5% of the fastest candidate\n"
    );

    print_ranking("INTERSECTION", rank_intersection());
    print_ranking("UNION", rank_union());
    print_ranking("SUBTRACTION", rank_subtraction());
    print_ranking("MULTI-WAY DISJOINT UNION", rank_multiway_union());
}
