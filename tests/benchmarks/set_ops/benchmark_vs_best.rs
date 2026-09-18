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

    fn name(self) -> &'static str {
        match self {
            Self::InterleavedZero => "interleaved_zero",
            Self::Overlap10 => "overlap_10pct",
            Self::Overlap50 => "overlap_50pct",
            Self::Subset => "subset",
        }
    }

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
    min: Duration,
    max: Duration,
}

impl TimingStats {
    fn spread_percent(self) -> f64 {
        if self.median.is_zero() {
            return 0.0;
        }

        (self.max.as_secs_f64() - self.min.as_secs_f64()) / self.median.as_secs_f64() * 100.0
    }
}

#[derive(Clone, Copy)]
struct NamedTiming {
    name: &'static str,
    stats: TimingStats,
}

fn timing_stats(mut values: Vec<Duration>) -> TimingStats {
    values.sort_unstable();

    TimingStats {
        min: values[0],
        median: values[values.len() / 2],
        max: values[values.len() - 1],
    }
}

fn ns(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e9
}

fn format_speedup(value: f64) -> String {
    if value >= 1.0 {
        format!("{value:.2}x faster")
    } else {
        format!("{:.2}x slower", 1.0 / value)
    }
}

fn format_gap_percent(value: f64) -> String {
    if value >= 0.0 {
        format!("+{value:.1}%")
    } else {
        format!("{value:.1}%")
    }
}

fn speedup(reference: TimingStats, candidate: TimingStats) -> f64 {
    reference.median.as_secs_f64() / candidate.median.as_secs_f64()
}

fn overhead_percent(candidate: TimingStats, reference: TimingStats) -> f64 {
    if reference.median.is_zero() {
        return 0.0;
    }

    (candidate.median.as_secs_f64() / reference.median.as_secs_f64() - 1.0) * 100.0
}

fn best_of(results: &[NamedTiming]) -> NamedTiming {
    results
        .iter()
        .copied()
        .min_by_key(|result| result.stats.median)
        .expect("at least one primitive candidate")
}

fn filtered(env: &str, value: &str) -> bool {
    std::env::var(env).is_ok_and(|filter| !value.contains(&filter))
}

fn strictly_increasing(values: &[u64]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

fn batch_size(input_bytes: usize) -> usize {
    let input_bytes = input_bytes.max(size_of::<u64>());
    (TARGET_BATCH_BYTES / input_bytes).clamp(MIN_BATCH, MAX_BATCH)
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

fn primitive_insert_binary(ids: &mut Vec<u64>, id: u64) {
    if ids.last().is_none_or(|&last| last < id) {
        ids.push(id);
        return;
    }

    let pos = ids.partition_point(|&existing| existing < id);
    ids.insert(pos, id);
}

fn primitive_insert_backscan(ids: &mut Vec<u64>, id: u64) {
    if ids.last().is_none_or(|&last| last < id) {
        ids.push(id);
        return;
    }

    let mut pos = ids.len();
    while pos > 0 && ids[pos - 1] > id {
        pos -= 1;
    }
    ids.insert(pos, id);
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
type InsertOp = fn(&mut Vec<u64>, u64);
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

fn print_vs_best(
    op: &str,
    context: &str,
    output: usize,
    production: TimingStats,
    baseline: TimingStats,
    primitives: &[NamedTiming],
) {
    let best = best_of(primitives);
    let candidates = primitives
        .iter()
        .map(|primitive| format!("{} {:.1} ns", primitive.name, ns(primitive.stats.median)))
        .collect::<Vec<_>>()
        .join(" | ");

    println!("{op} | {context} | out={output}");
    println!(
        "  prod {:>10.1} ns (spread {:>5.1}%) | baseline {:>10.1} ns | {}",
        ns(production.median),
        production.spread_percent(),
        ns(baseline.median),
        format_speedup(speedup(baseline, production)),
    );
    println!(
        "  best {:<11} {:>10.1} ns | gap {} | {}",
        best.name,
        ns(best.stats.median),
        format_gap_percent(overhead_percent(production, best.stats)),
        candidates,
    );
}

#[test]
#[ignore = "production intersection vs baseline and best primitive"]
fn bench_intersection() {
    println!("\n=== INTERSECTION — PRODUCTION VS BEST PRIMITIVE ===");
    let primitive_ops: [(&str, InPlaceOp); 3] = [
        ("linear", primitive_intersect_linear),
        ("binary", primitive_intersect_binary),
        ("gallop", primitive_intersect_gallop),
    ];

    for &small in SMALLER_SIZES {
        for &ratio in RATIOS {
            let large = small.saturating_mul(ratio);
            if large > MAX_POSTING_LEN {
                continue;
            }

            let case_name = format!("small{small}_ratio{ratio}");
            if filtered("TSDB_SET_CASE", &case_name) {
                continue;
            }

            for pattern in Pattern::ALL {
                let (a, b) = build_pair(small, large, pattern);

                let mut expected = a.clone();
                primitive_intersect_linear(&mut expected, &b);

                let mut check = a.clone();
                intersect_in_place(&mut check, &b);
                assert_eq!(check, expected);

                let production = measure_in_place(intersect_in_place, &a, &b);
                let baseline = measure_in_place(primitive_intersect_linear, &a, &b);
                let primitives = primitive_ops
                    .iter()
                    .map(|&(name, run)| {
                        let mut check = a.clone();
                        run(&mut check, &b);
                        assert_eq!(check, expected);
                        NamedTiming {
                            name,
                            stats: measure_in_place(run, &a, &b),
                        }
                    })
                    .collect::<Vec<_>>();

                let context = format!("small={small} ratio={ratio} pattern={}", pattern.name());

                print_vs_best(
                    "intersection",
                    &context,
                    expected.len(),
                    production,
                    baseline,
                    &primitives,
                );
            }
        }
    }
}

#[test]
#[ignore = "production union vs baseline and best primitive"]
fn bench_union() {
    println!("\n=== UNION — PRODUCTION VS BEST PRIMITIVE ===");
    let primitive_ops: [(&str, UnionOp); 2] = [
        ("linear", primitive_union_linear),
        ("gallop", primitive_union_gallop),
    ];

    for &small in SMALLER_SIZES {
        for &ratio in RATIOS {
            let large = small.saturating_mul(ratio);
            if large > MAX_POSTING_LEN {
                continue;
            }

            let case_name = format!("small{small}_ratio{ratio}");
            if filtered("TSDB_SET_CASE", &case_name) {
                continue;
            }

            for pattern in Pattern::ALL {
                let (a, b) = build_pair(small, large, pattern);
                let expected = primitive_union_linear(&a, &b);
                assert_eq!(union_sorted(&a, &b), expected);

                let production = measure_union(union_sorted, &a, &b);
                let baseline = measure_union(primitive_union_linear, &a, &b);
                let primitives = primitive_ops
                    .iter()
                    .map(|&(name, run)| {
                        let check = run(&a, &b);
                        assert_eq!(check, expected);
                        NamedTiming {
                            name,
                            stats: measure_union(run, &a, &b),
                        }
                    })
                    .collect::<Vec<_>>();

                let context = format!("small={small} ratio={ratio} pattern={}", pattern.name());

                print_vs_best(
                    "union",
                    &context,
                    expected.len(),
                    production,
                    baseline,
                    &primitives,
                );
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Orientation {
    BLarger,
    ALarger,
}

impl Orientation {
    const ALL: [Self; 2] = [Self::BLarger, Self::ALarger];

    fn name(self) -> &'static str {
        match self {
            Self::BLarger => "b_larger",
            Self::ALarger => "a_larger",
        }
    }

    fn lengths(self, small: usize, large: usize) -> (usize, usize) {
        match self {
            Self::BLarger => (small, large),
            Self::ALarger => (large, small),
        }
    }
}

#[test]
#[ignore = "production subtraction vs baseline and best primitive"]
fn bench_subtraction() {
    println!("\n=== SUBTRACTION — PRODUCTION VS BEST PRIMITIVE ===");
    let primitive_ops: [(&str, InPlaceOp); 4] = [
        ("linear", primitive_subtract_linear),
        ("binary", primitive_subtract_binary),
        ("seek", primitive_subtract_seek),
        ("sparse", primitive_subtract_sparse),
    ];

    for orientation in Orientation::ALL {
        for &small in SMALLER_SIZES {
            for &ratio in RATIOS {
                let large = small.saturating_mul(ratio);
                if large > MAX_POSTING_LEN {
                    continue;
                }

                let (a_len, b_len) = orientation.lengths(small, large);
                let case_name = format!("{}_small{small}_ratio{ratio}", orientation.name());
                if filtered("TSDB_SET_CASE", &case_name) {
                    continue;
                }

                for pattern in Pattern::ALL {
                    let (a, b) = build_pair(a_len, b_len, pattern);

                    let mut expected = a.clone();
                    primitive_subtract_linear(&mut expected, &b);

                    let mut check = a.clone();
                    subtract_in_place(&mut check, &b);
                    assert_eq!(check, expected);

                    let production = measure_in_place(subtract_in_place, &a, &b);
                    let baseline = measure_in_place(primitive_subtract_linear, &a, &b);
                    let primitives = primitive_ops
                        .iter()
                        .map(|&(name, run)| {
                            let mut check = a.clone();
                            run(&mut check, &b);
                            assert_eq!(check, expected);
                            NamedTiming {
                                name,
                                stats: measure_in_place(run, &a, &b),
                            }
                        })
                        .collect::<Vec<_>>();

                    let context = format!(
                        "orientation={} small={small} ratio={ratio} pattern={}",
                        orientation.name(),
                        pattern.name()
                    );

                    print_vs_best(
                        "subtraction",
                        &context,
                        expected.len(),
                        production,
                        baseline,
                        &primitives,
                    );
                }
            }
        }
    }
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

#[test]
#[ignore = "production disjoint multi-union vs best primitive"]
fn bench_multiway_union_disjoint() {
    println!("\n=== MULTI-WAY DISJOINT UNION — PRODUCTION VS BEST PRIMITIVE ===");
    const LIST_COUNTS: &[usize] = &[2, 8, 16, 32, 64];
    const PER_LIST: &[usize] = &[64, 256, 1_024, 4_096];

    let primitive_ops: [(&str, ManyOp); 2] = [
        ("pairwise", primitive_many_pairwise),
        ("concat_sort", primitive_many_concat_sort),
    ];

    for &list_count in LIST_COUNTS {
        for &per_list in PER_LIST {
            if list_count * per_list > MAX_POSTING_LEN {
                continue;
            }

            let owned = build_disjoint_many(list_count, per_list);
            let lists = owned.iter().map(Vec::as_slice).collect::<Vec<_>>();
            let expected = primitive_many_concat_sort(&lists);
            assert_eq!(union_many_disjoint(&lists), expected);

            let production = measure_many(union_many_disjoint, &lists);
            let baseline = measure_many(primitive_many_concat_sort, &lists);
            let primitives = primitive_ops
                .iter()
                .map(|&(name, run)| {
                    let check = run(&lists);
                    assert_eq!(check, expected);
                    NamedTiming {
                        name,
                        stats: measure_many(run, &lists),
                    }
                })
                .collect::<Vec<_>>();

            let context = format!("lists={list_count} per_list={per_list}");
            print_vs_best(
                "union_many_disjoint",
                &context,
                expected.len(),
                production,
                baseline,
                &primitives,
            );
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct InsertCase {
    name: &'static str,
    shifted: usize,
}

const INSERT_CASES: &[InsertCase] = &[
    InsertCase {
        name: "append",
        shifted: 0,
    },
    InsertCase {
        name: "back_1",
        shifted: 1,
    },
    InsertCase {
        name: "back_4",
        shifted: 4,
    },
    InsertCase {
        name: "back_16",
        shifted: 16,
    },
    InsertCase {
        name: "back_64",
        shifted: 64,
    },
    InsertCase {
        name: "back_256",
        shifted: 256,
    },
    InsertCase {
        name: "back_1024",
        shifted: 1_024,
    },
    InsertCase {
        name: "back_4096",
        shifted: 4_096,
    },
];

const INSERT_SIZES: &[usize] = &[64, 1_024, 65_536, 262_144];

fn insertion_value(len: usize, shifted: usize) -> u64 {
    assert!(shifted <= len);
    let insertion_index = len - shifted;
    insertion_index as u64 * 2 + 1
}

fn measure_insert(
    run: InsertOp,
    base: &[u64],
    value: u64,
    expected_position: usize,
) -> TimingStats {
    let bytes = size_of_val(base) + size_of::<u64>();
    let batch = batch_size(bytes);
    let mut runs = Vec::with_capacity(RUNS_PER_CASE);

    for _ in 0..RUNS_PER_CASE {
        let mut inputs = (0..batch)
            .map(|_| {
                let mut values = Vec::with_capacity(base.len() + 1);
                values.extend_from_slice(base);
                values
            })
            .collect::<Vec<_>>();

        black_box(&mut inputs);
        let started = Instant::now();
        let mut checksum = 0u64;

        for values in &mut inputs {
            run(black_box(values), black_box(value));
            checksum = checksum
                .wrapping_add(values.len() as u64)
                .wrapping_add(values[expected_position]);
            black_box(&*values);
        }

        black_box(checksum);
        runs.push(started.elapsed().div_f64(batch as f64));
    }

    timing_stats(runs)
}

#[test]
#[ignore = "production insertion vs best primitive"]
fn bench_sorted_insertion() {
    println!("\n=== SORTED INSERTION — PRODUCTION VS BEST PRIMITIVE ===");
    let primitive_ops: [(&str, InsertOp); 2] = [
        ("binary", primitive_insert_binary),
        ("backscan", primitive_insert_backscan),
    ];

    for &len in INSERT_SIZES {
        let base = (0..len as u64)
            .map(|index| (index + 1) * 2)
            .collect::<Vec<_>>();

        for case in INSERT_CASES {
            if case.shifted > len || filtered("TSDB_INSERT_POSITION", case.name) {
                continue;
            }

            let value = insertion_value(len, case.shifted);
            let expected_position = len - case.shifted;

            let mut expected = Vec::with_capacity(base.len() + 1);
            expected.extend_from_slice(&base);
            primitive_insert_binary(&mut expected, value);

            let mut check = Vec::with_capacity(base.len() + 1);
            check.extend_from_slice(&base);
            insert_sorted(&mut check, value);
            assert_eq!(check, expected);

            let production = measure_insert(insert_sorted, &base, value, expected_position);
            let baseline = measure_insert(primitive_insert_binary, &base, value, expected_position);
            let primitives = primitive_ops
                .iter()
                .map(|&(name, run)| NamedTiming {
                    name,
                    stats: measure_insert(run, &base, value, expected_position),
                })
                .collect::<Vec<_>>();

            let best = best_of(&primitives);
            let candidates = primitives
                .iter()
                .map(|primitive| format!("{} {:.1} ns", primitive.name, ns(primitive.stats.median)))
                .collect::<Vec<_>>()
                .join(", ");

            println!(
                "insert | len={len} pos={} shifted={}",
                case.name, case.shifted,
            );
            println!(
                "  prod {:>10.1} ns (spread {:>5.1}%) | binary {:>10.1} ns | {}",
                ns(production.median),
                production.spread_percent(),
                ns(baseline.median),
                format_speedup(speedup(baseline, production)),
            );
            println!(
                "  best {:<8} {:>10.1} ns | gap {} | {}",
                best.name,
                ns(best.stats.median),
                format_gap_percent(overhead_percent(production, best.stats)),
                candidates,
            );
        }
    }
}
