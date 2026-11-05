use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};
use xxhash_rust::xxh3::xxh3_64_with_seed;

#[derive(PartialEq, Eq, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct LabelOffset {
    name_start: u32,
    value_start: u32,
}

impl LabelOffset {
    pub fn new(name_start: u32, value_start: u32) -> Self {
        Self {
            name_start,
            value_start,
        }
    }

    pub fn name_start(self) -> u32 {
        self.name_start
    }

    pub fn value_start(self) -> u32 {
        self.value_start
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(into = "RawLabelSet", try_from = "RawLabelSet")]
pub struct LabelSet {
    data: Box<[u8]>,
    offsets: Box<[LabelOffset]>,
    fingerprint: u64,
}

#[derive(Serialize, Deserialize)]
struct RawLabelSet {
    data: Vec<u8>,
    offsets: Vec<LabelOffset>,
}

impl From<LabelSet> for RawLabelSet {
    fn from(set: LabelSet) -> Self {
        Self {
            data: set.data.into_vec(),
            offsets: set.offsets.into_vec(),
        }
    }
}

impl TryFrom<RawLabelSet> for LabelSet {
    type Error = &'static str;

    fn try_from(raw: RawLabelSet) -> Result<Self, Self::Error> {
        LabelSet::from_raw_parts(&raw.data, raw.offsets).ok_or("invalid label set")
    }
}

impl LabelSet {
    pub fn from_sorted_pairs(pairs: &[(&str, &str)]) -> Self {
        debug_assert!(
            pairs.windows(2).all(|w| w[0].0 < w[1].0),
            "sorted, unique names"
        );
        let len: usize = pairs.iter().map(|(n, v)| n.len() + v.len()).sum();
        assert!(len <= u32::MAX as usize, "label set too large");

        let mut data = Vec::with_capacity(len);
        let mut offsets = Vec::with_capacity(pairs.len());
        for (name, value) in pairs {
            let name_start = data.len() as u32;
            data.extend_from_slice(name.as_bytes());
            let value_start = data.len() as u32;
            data.extend_from_slice(value.as_bytes());
            offsets.push(LabelOffset {
                name_start,
                value_start,
            });
        }
        Self::from_valid_parts(data.into_boxed_slice(), offsets.into_boxed_slice())
    }

    /// Packed representation for byte-level encoders (WAL).
    pub fn raw_parts(&self) -> (&[u8], &[LabelOffset]) {
        (&self.data, &self.offsets)
    }

    pub fn from_raw_parts(data: &[u8], offsets: Vec<LabelOffset>) -> Option<Self> {
        if offsets.first().is_some_and(|o| o.name_start != 0)
            || (offsets.is_empty() && !data.is_empty())
        {
            return None;
        }

        let text = std::str::from_utf8(data).ok()?;
        let ends = offsets
            .iter()
            .skip(1)
            .map(|o| o.name_start)
            .chain([u32::try_from(data.len()).ok()?]);
        let mut prev_name: Option<&str> = None;

        for (o, value_end) in offsets.iter().zip(ends) {
            let (n, v, e) = (
                o.name_start as usize,
                o.value_start as usize,
                value_end as usize,
            );
            if !(n <= v && v <= e) || !text.is_char_boundary(n) || !text.is_char_boundary(v) {
                return None;
            }
            let name = &text[n..v];
            if prev_name.is_some_and(|p| p >= name) {
                return None;
            }
            prev_name = Some(name);
        }

        Some(Self::from_valid_parts(
            data.into(),
            offsets.into_boxed_slice(),
        ))
    }

    fn from_valid_parts(data: Box<[u8]>, offsets: Box<[LabelOffset]>) -> Self {
        let mut set = Self {
            data,
            offsets,
            fingerprint: 0,
        };

        let seed = set.offsets.iter().fold(set.offsets.len() as u64, |h, o| {
            (h ^ ((o.name_start as u64) << 32 | o.value_start as u64))
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        });

        set.fingerprint = xxh3_64_with_seed(&set.data, seed);

        set
    }

    pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut pairs: Vec<(&str, &str)> = pairs.into_iter().collect();

        pairs.reverse();
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        pairs.dedup_by(|a, b| a.0 == b.0);

        Self::from_sorted_pairs(&pairs)
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.iter().find(|(n, _)| *n == name).map(|(_, v)| v)
    }

    #[inline]
    pub fn iter(&self) -> LabelIter<'_> {
        LabelIter {
            data: &self.data,
            offsets: &self.offsets,
            index: 0,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }
}

impl Default for LabelSet {
    fn default() -> Self {
        Self::from_sorted_pairs(&[])
    }
}

impl PartialEq for LabelSet {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.fingerprint == other.fingerprint
            && self.offsets == other.offsets
            && self.data == other.data
    }
}

impl Eq for LabelSet {}

impl Hash for LabelSet {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.fingerprint);
    }
}

pub struct LabelIter<'a> {
    data: &'a [u8],
    offsets: &'a [LabelOffset],
    index: usize,
}

impl<'a> Iterator for LabelIter<'a> {
    type Item = (&'a str, &'a str);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let index = self.index;

        if index == self.offsets.len() {
            return None;
        }

        let current = unsafe { *self.offsets.get_unchecked(index) };

        let name_start = current.name_start as usize;
        let value_start = current.value_start as usize;

        let next_index = index + 1;

        let value_end = if next_index == self.offsets.len() {
            self.data.len()
        } else {
            unsafe { self.offsets.get_unchecked(next_index).name_start as usize }
        };

        self.index = next_index;

        unsafe {
            let name_bytes = self.data.get_unchecked(name_start..value_start);
            let value_bytes = self.data.get_unchecked(value_start..value_end);

            Some((
                std::str::from_utf8_unchecked(name_bytes),
                std::str::from_utf8_unchecked(value_bytes),
            ))
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.offsets.len() - self.index;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for LabelIter<'_> {}
impl std::iter::FusedIterator for LabelIter<'_> {}

impl<'a> IntoIterator for &'a LabelSet {
    type Item = (&'a str, &'a str);
    type IntoIter = LabelIter<'a>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[derive(Debug, Clone, PartialEq, Copy)]
pub struct Sample {
    /// in miliseconds since epoch
    pub timestamp: u64,
    pub value: f64,
}

impl Sample {
    pub fn new(timestamp: u64, value: f64) -> Self {
        Self { timestamp, value }
    }

    pub fn in_timerange(self, range: TimeRange) -> bool {
        self.timestamp >= range.start && self.timestamp <= range.end
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct SeriesId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    /// in miliseconds since epoch
    pub start: u64,
    /// in miliseconds since epoch
    pub end: u64,
}

impl TimeRange {
    pub fn new(start: u64, end: u64) -> Self {
        Self { start, end }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum MatcherOperator {
    Equal,
    NotEqual,
}

/// For now same shit as Label, but will become helpful when we implement matchers and operators
#[derive(Debug, Clone)]
pub struct Matcher {
    pub name: String,
    pub value: String,
    pub operator: MatcherOperator,
}

impl Matcher {
    pub fn new(
        name: impl Into<String>,
        value: impl Into<String>,
        operator: MatcherOperator,
    ) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            operator,
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    fn label_set(pairs: &[(&str, &str)]) -> LabelSet {
        LabelSet::from_pairs(pairs.iter().copied())
    }

    #[test]
    fn insert_overwrites_existing_value_for_same_name() {
        let set = label_set(&[("host", "a"), ("host", "b")]);

        assert_eq!(set.get("host"), Some("b"));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn equality_is_independent_of_insertion_order() {
        let set_a = label_set(&[
            ("zone", "eu"),
            ("__name__", "http_requests"),
            ("method", "get"),
        ]);
        let set_b = label_set(&[
            ("method", "get"),
            ("zone", "eu"),
            ("__name__", "http_requests"),
        ]);

        assert_eq!(set_a, set_b);
    }

    #[test]
    fn iteration_is_sorted_by_name_regardless_of_insertion_order() {
        let set = label_set(&[
            ("zone", "eu"),
            ("__name__", "http_requests"),
            ("method", "get"),
        ]);

        let names: Vec<&str> = set.into_iter().map(|(name, _)| name).collect();

        assert_eq!(names, vec!["__name__", "method", "zone"]);
    }

    #[test]
    fn metric_name_returns_name_label_value() {
        let set = label_set(&[("method", "get"), ("__name__", "http_requests")]);
        assert_eq!(set.get("__name__"), Some("http_requests"));
    }

    #[test]
    fn metric_name_absent_returns_none() {
        let set = label_set(&[("method", "get")]);
        assert_eq!(set.get("__name__"), None);
    }

    #[test]
    fn get_returns_value_for_existing_label() {
        let set = label_set(&[("method", "get"), ("zone", "eu")]);
        assert_eq!(set.get("zone"), Some("eu"));
    }

    #[test]
    fn get_returns_none_for_missing_label() {
        let set = label_set(&[("method", "get")]);
        assert_eq!(set.get("zone"), None);
    }

    #[test]
    fn from_labels_builds_equivalent_set_to_insert() {
        let via_labels =
            LabelSet::from_sorted_pairs(&[("__name__", "http_requests"), ("method", "get")]);
        let via_insert = label_set(&[("__name__", "http_requests"), ("method", "get")]);

        assert_eq!(via_labels, via_insert);
    }

    #[test]
    fn from_labels_later_duplicate_overwrites_earlier() {
        let set = LabelSet::from_pairs([("host", "a"), ("host", "b")]);

        assert_eq!(set.get("host"), Some("b"));
        assert_eq!(set.len(), 1);
    }
}
