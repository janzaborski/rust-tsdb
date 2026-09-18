use std::cmp::Ordering;

const INTERSECT_CLIP_RATIO: usize = 16;
const INTERSECT_LARGE_CLIP_RATIO: usize = 24;
const INTERSECT_SPECIALIZED_RATIO: usize = 12;
const INTERSECT_BINARY_MAX_DRIVER: usize = 64;
const INTERSECT_MEDIUM_DRIVER_MAX: usize = 256;
const INTERSECT_LARGE_GALLOP_RATIO: usize = 20;

const UNION_RAW_GALLOP_RATIO: usize = 24;
const UNION_EFFECTIVE_GALLOP_RATIO: usize = 16;

const SUBTRACT_CLIP_RATIO: usize = 16;
const SUBTRACT_SPECIALIZED_RATIO: usize = 12;
const SUBTRACT_BINARY_MAX_DRIVER: usize = 64;

pub const TAIL_LINEAR_SCAN: usize = 256;

// Helpers

#[derive(Clone, Copy)]
pub struct Overlap {
    a_start: usize,
    a_end: usize,
    b_start: usize,
    b_end: usize,
}

#[inline]
pub fn disjoint<T: Ord + Copy>(a: &[T], b: &[T]) -> bool {
    a.is_empty()
        || b.is_empty()
        || a[0] > *b.last().expect("b is non-empty")
        || b[0] > *a.last().expect("a is non-empty")
}

#[inline]
pub fn overlap_bounds<T: Ord + Copy>(a: &[T], b: &[T]) -> Option<Overlap> {
    if disjoint(a, b) {
        return None;
    }

    let a_first = a[0];
    let a_last = *a.last().expect("a is non-empty");
    let b_first = b[0];
    let b_last = *b.last().expect("b is non-empty");

    let a_start = if a_first < b_first {
        a.partition_point(|&value| value < b_first)
    } else {
        0
    };
    let a_end = if a_last > b_last {
        a.partition_point(|&value| value <= b_last)
    } else {
        a.len()
    };
    let b_start = if b_first < a_first {
        b.partition_point(|&value| value < a_first)
    } else {
        0
    };
    let b_end = if b_last > a_last {
        b.partition_point(|&value| value <= a_last)
    } else {
        b.len()
    };

    Some(Overlap {
        a_start,
        a_end,
        b_start,
        b_end,
    })
}

#[inline]
pub fn clip_to_value_range<T: Ord + Copy>(values: &[T], low: T, high: T) -> (usize, usize) {
    let start = if values[0] < low {
        values.partition_point(|&value| value < low)
    } else {
        0
    };
    let end = if *values.last().expect("values is non-empty") > high {
        values.partition_point(|&value| value <= high)
    } else {
        values.len()
    };

    (start, end)
}

// First index at or after `start` whose value is >= `target`.
#[inline]
pub fn gallop<T: Ord + Copy>(haystack: &[T], start: usize, target: T) -> usize {
    let end = haystack.len();

    if start >= end {
        return end;
    }
    if haystack[start] >= target {
        return start;
    }

    let mut lo = start;
    let mut step = 1usize;

    while step < end - lo && haystack[lo + step] < target {
        lo += step;
        step = step.saturating_mul(2);
    }

    let hi = lo.saturating_add(step).saturating_add(1).min(end);
    (lo + 1) + haystack[lo + 1..hi].partition_point(|&value| value < target)
}

// Intersection

pub fn intersect_linear_range<T: Ord + Copy>(
    a: &mut Vec<T>,
    a_start: usize,
    a_end: usize,
    b: &[T],
    b_start: usize,
    b_end: usize,
) {
    let (mut write, mut i, mut j) = (0usize, a_start, b_start);

    while i < a_end && j < b_end {
        match a[i].cmp(&b[j]) {
            Ordering::Less => i += 1,
            Ordering::Greater => j += 1,
            Ordering::Equal => {
                a[write] = a[i];
                write += 1;
                i += 1;
                j += 1;
            }
        }
    }

    a.truncate(write);
}

pub fn intersect_binary_range<T: Ord + Copy>(
    a: &mut Vec<T>,
    a_end: usize,
    b: &[T],
    b_start: usize,
    b_end: usize,
) {
    let b = &b[b_start..b_end];
    let mut write = 0usize;

    for i in 0..a_end {
        let value = a[i];
        if b.binary_search(&value).is_ok() {
            a[write] = value;
            write += 1;
        }
    }

    a.truncate(write);
}

pub fn intersect_gallop_range<T: Ord + Copy>(
    a: &mut Vec<T>,
    a_end: usize,
    b: &[T],
    b_start: usize,
    b_end: usize,
) {
    let b = &b[b_start..b_end];
    let mut cursor = 0usize;
    let mut write = 0usize;

    for i in 0..a_end {
        let value = a[i];
        cursor = gallop(b, cursor, value);

        if cursor >= b.len() {
            break;
        }

        if b[cursor] == value {
            a[write] = value;
            write += 1;
            cursor += 1;
        }
    }

    a.truncate(write);
}

/// Intersect `a` with `b` in place
pub fn intersect_in_place<T: Ord + Copy>(a: &mut Vec<T>, b: &[T]) {
    if a.is_empty() || b.is_empty() {
        a.clear();
        return;
    }

    if disjoint(a, b) {
        a.clear();
        return;
    }

    let raw_ratio = b.len() / a.len();

    if raw_ratio < INTERSECT_CLIP_RATIO
        || (a.len() > INTERSECT_MEDIUM_DRIVER_MAX && raw_ratio < INTERSECT_LARGE_CLIP_RATIO)
    {
        let a_end = a.len();
        intersect_linear_range(a, 0, a_end, b, 0, b.len());
        return;
    }

    let (b_start, b_end) = clip_to_value_range(b, a[0], *a.last().expect("a is non-empty"));
    let b_len = b_end - b_start;

    if b_len == 0 {
        a.clear();
        return;
    }

    let effective_ratio = if b_len >= a.len() { b_len / a.len() } else { 1 };
    let a_end = a.len();

    if effective_ratio < INTERSECT_SPECIALIZED_RATIO {
        intersect_linear_range(a, 0, a_end, b, b_start, b_end);
    } else if a.len() <= INTERSECT_BINARY_MAX_DRIVER {
        intersect_binary_range(a, a_end, b, b_start, b_end);
    } else if a.len() <= INTERSECT_MEDIUM_DRIVER_MAX
        || effective_ratio >= INTERSECT_LARGE_GALLOP_RATIO
    {
        intersect_gallop_range(a, a_end, b, b_start, b_end);
    } else {
        intersect_linear_range(a, 0, a_end, b, b_start, b_end);
    }
}

// Union

pub fn union_linear_into<T: Ord + Copy>(a: &[T], b: &[T], out: &mut Vec<T>) {
    let (mut i, mut j) = (0usize, 0usize);

    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            Ordering::Less => {
                out.push(a[i]);
                i += 1;
            }
            Ordering::Greater => {
                out.push(b[j]);
                j += 1;
            }
            Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }

    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
}

pub fn union_gallop_runs_into<T: Ord + Copy>(a: &[T], b: &[T], out: &mut Vec<T>) {
    let (mut i, mut j) = (0usize, 0usize);

    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            Ordering::Less => {
                let next = gallop(a, i, b[j]);
                out.extend_from_slice(&a[i..next]);
                i = next;
            }
            Ordering::Greater => {
                let next = gallop(b, j, a[i]);
                out.extend_from_slice(&b[j..next]);
                j = next;
            }
            Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }

    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
}

pub fn union_into<T: Ord + Copy>(a: &[T], b: &[T], out: &mut Vec<T>) {
    if a.is_empty() {
        out.extend_from_slice(b);
        return;
    }
    if b.is_empty() {
        out.extend_from_slice(a);
        return;
    }

    if *a.last().expect("a is non-empty") < b[0] {
        out.extend_from_slice(a);
        out.extend_from_slice(b);
        return;
    }
    if *b.last().expect("b is non-empty") < a[0] {
        out.extend_from_slice(b);
        out.extend_from_slice(a);
        return;
    }

    let smaller = a.len().min(b.len());
    let larger = a.len().max(b.len());

    if larger / smaller < UNION_RAW_GALLOP_RATIO {
        union_linear_into(a, b, out);
        return;
    }

    let overlap = overlap_bounds(a, b).expect("ranges overlap");

    out.extend_from_slice(&a[..overlap.a_start]);
    out.extend_from_slice(&b[..overlap.b_start]);

    let a_core = &a[overlap.a_start..overlap.a_end];
    let b_core = &b[overlap.b_start..overlap.b_end];
    let core_small = a_core.len().min(b_core.len());
    let core_large = a_core.len().max(b_core.len());

    if core_small == 0 {
        out.extend_from_slice(a_core);
        out.extend_from_slice(b_core);
    } else if core_large / core_small >= UNION_EFFECTIVE_GALLOP_RATIO {
        union_gallop_runs_into(a_core, b_core, out);
    } else {
        union_linear_into(a_core, b_core, out);
    }

    out.extend_from_slice(&a[overlap.a_end..]);
    out.extend_from_slice(&b[overlap.b_end..]);
}

/// Union into a sorted deduplicated set
pub fn union_sorted<T: Ord + Copy>(a: &[T], b: &[T]) -> Vec<T> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    union_into(a, b, &mut out);
    out
}

// Subtraction

pub fn subtract_linear_range<T: Ord + Copy>(
    a: &mut [T],
    a_start: usize,
    a_end: usize,
    b: &[T],
    b_start: usize,
    b_end: usize,
) -> usize {
    let (mut write, mut i, mut j) = (a_start, a_start, b_start);

    while i < a_end && j < b_end {
        match a[i].cmp(&b[j]) {
            Ordering::Less => {
                a[write] = a[i];
                write += 1;
                i += 1;
            }
            Ordering::Greater => j += 1,
            Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
    }

    if i < a_end {
        let tail = a_end - i;
        if write != i {
            a.copy_within(i..a_end, write);
        }
        write += tail;
    }

    write
}

pub fn subtract_binary_range<T: Ord + Copy>(
    a: &mut [T],
    a_end: usize,
    b: &[T],
    b_start: usize,
    b_end: usize,
) -> usize {
    let b = &b[b_start..b_end];
    let mut write = 0usize;

    for i in 0..a_end {
        let value = a[i];
        if b.binary_search(&value).is_err() {
            a[write] = value;
            write += 1;
        }
    }

    write
}

pub fn subtract_seek_range<T: Ord + Copy>(
    a: &mut [T],
    a_end: usize,
    b: &[T],
    b_start: usize,
    b_end: usize,
) -> usize {
    let b = &b[b_start..b_end];
    let mut write = 0usize;
    let mut cursor = 0usize;

    for i in 0..a_end {
        let value = a[i];

        if cursor < b.len() && b[cursor] < value {
            cursor = gallop(b, cursor, value);
        }

        if cursor >= b.len() || b[cursor] != value {
            a[write] = value;
            write += 1;
        } else {
            cursor += 1;
        }
    }

    write
}

pub fn subtract_sparse_range<T: Ord + Copy>(
    a: &mut [T],
    a_start: usize,
    a_end: usize,
    b: &[T],
) -> usize {
    let mut read = a_start;
    let mut write = a_start;

    for &drop_value in b {
        if read >= a_end {
            break;
        }

        let found = gallop(&a[..a_end], read, drop_value);

        if found > read {
            let run_len = found - read;
            if write != read {
                a.copy_within(read..found, write);
            }
            write += run_len;
        }

        if found < a_end && a[found] == drop_value {
            read = found + 1;
        } else {
            read = found;
        }
    }

    if read < a_end {
        let tail = a_end - read;
        if write != read {
            a.copy_within(read..a_end, write);
        }
        write += tail;
    }

    write
}

pub fn finish_subtraction<T: Ord + Copy>(
    a: &mut Vec<T>,
    core_write_end: usize,
    original_core_end: usize,
    original_len: usize,
) {
    if original_core_end < original_len {
        let suffix_len = original_len - original_core_end;
        if core_write_end != original_core_end {
            a.copy_within(original_core_end..original_len, core_write_end);
        }
        a.truncate(core_write_end + suffix_len);
    } else {
        a.truncate(core_write_end);
    }
}

/// Subtract b from a
pub fn subtract_in_place<T: Ord + Copy>(a: &mut Vec<T>, b: &[T]) {
    if a.is_empty() || b.is_empty() || disjoint(a, b) {
        return;
    }

    if b.len() >= a.len() {
        let raw_ratio = b.len() / a.len();

        if raw_ratio < SUBTRACT_CLIP_RATIO {
            let original_len = a.len();
            let write_end = subtract_linear_range(a, 0, original_len, b, 0, b.len());
            a.truncate(write_end);
            return;
        }

        let (b_start, b_end) = clip_to_value_range(b, a[0], *a.last().expect("a is non-empty"));
        let b_len = b_end - b_start;

        if b_len == 0 {
            return;
        }

        let effective_ratio = if b_len >= a.len() { b_len / a.len() } else { 1 };
        let original_len = a.len();

        let write_end = if effective_ratio < SUBTRACT_SPECIALIZED_RATIO {
            subtract_linear_range(a, 0, original_len, b, b_start, b_end)
        } else if a.len() <= SUBTRACT_BINARY_MAX_DRIVER {
            subtract_binary_range(a, original_len, b, b_start, b_end)
        } else {
            subtract_seek_range(a, original_len, b, b_start, b_end)
        };

        a.truncate(write_end);
        return;
    }

    let raw_ratio = a.len() / b.len();

    if raw_ratio < SUBTRACT_CLIP_RATIO {
        let original_len = a.len();
        let write_end = subtract_linear_range(a, 0, original_len, b, 0, b.len());
        a.truncate(write_end);
        return;
    }

    let original_len = a.len();
    let (a_start, a_end) = clip_to_value_range(a, b[0], *b.last().expect("b is non-empty"));
    let a_len = a_end - a_start;

    if a_len == 0 {
        return;
    }

    let effective_ratio = if a_len >= b.len() { a_len / b.len() } else { 1 };

    let write_end = if effective_ratio >= SUBTRACT_SPECIALIZED_RATIO {
        subtract_sparse_range(a, a_start, a_end, b)
    } else {
        subtract_linear_range(a, a_start, a_end, b, 0, b.len())
    };

    finish_subtraction(a, write_end, a_end, original_len);
}

// Multi-way union

pub fn union_many_pairwise_reuse<T: Ord + Copy>(lists: &[&[T]], total: usize) -> Vec<T> {
    let mut iter = lists.iter().copied();
    let Some(first) = iter.next() else {
        return Vec::new();
    };

    let mut current = Vec::with_capacity(total);
    current.extend_from_slice(first);

    let mut scratch = Vec::with_capacity(total);

    for posting in iter {
        scratch.clear();
        union_into(&current, posting, &mut scratch);
        std::mem::swap(&mut current, &mut scratch);
    }

    current
}

pub fn union_many_disjoint_concat_sort<T: Ord + Copy>(lists: &[&[T]], total: usize) -> Vec<T> {
    let mut out = Vec::with_capacity(total);

    for posting in lists {
        out.extend_from_slice(posting);
    }

    out.sort_unstable();
    out
}

/// Union many disjoint lists at once
pub fn union_many_disjoint<T: Ord + Copy>(lists: &[&[T]]) -> Vec<T> {
    match lists.len() {
        0 => return Vec::new(),
        1 => return lists[0].to_vec(),
        _ => {}
    }

    let total = lists.iter().map(|posting| posting.len()).sum::<usize>();
    let average_len = total / lists.len();

    if lists.len() >= 32 || (lists.len() >= 16 && average_len <= 256) {
        union_many_disjoint_concat_sort(lists, total)
    } else {
        union_many_pairwise_reuse(lists, total)
    }
}

// Insertion

#[inline]
pub fn sorted_insert_position<T: Ord + Copy>(ids: &[T], id: T) -> Option<usize> {
    if ids.last().is_none_or(|&last| last < id) {
        return None;
    }

    let len = ids.len();
    let tail_start = len.saturating_sub(TAIL_LINEAR_SCAN);
    let mut pos = len;

    while pos > tail_start && ids[pos - 1] > id {
        pos -= 1;
    }

    if pos > tail_start || tail_start == 0 {
        return Some(pos);
    }

    if ids[tail_start - 1] < id {
        return Some(tail_start);
    }

    Some(ids[..tail_start].partition_point(|&existing| existing < id))
}

/// Insert keeping the sorted invariant
#[inline]
pub fn insert_sorted<T: Ord + Copy>(ids: &mut Vec<T>, id: T) {
    match sorted_insert_position(ids, id) {
        Some(pos) => {
            ids.insert(pos, id);
        }
        None => ids.push(id),
    }
}
