use super::error::{DbError, StorageError};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Label {
    pub name: String,
    pub value: String,
}

impl Label {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct LabelSet(BTreeMap<String, String>);

impl LabelSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_labels(labels: impl IntoIterator<Item = Label>) -> Self {
        let mut set = Self::new();
        for label in labels {
            set.insert_label(label);
        }
        set
    }

    /// Returns metric name if it exists (that is label named __name__)
    pub fn metric_name(&self) -> Option<&str> {
        self.0.get("__name__").map(String::as_str)
    }

    /// Returns the value of the label with the given name, if it exists.   
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.0.insert(name.into(), value.into());
    }

    pub fn insert_label(&mut self, label: Label) {
        self.0.insert(label.name, label.value);
    }

    pub fn remove(&mut self, name: &str) -> Option<String> {
        self.0.remove(name)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'a> IntoIterator for &'a LabelSet {
    type Item = (&'a String, &'a String);
    type IntoIter = std::collections::btree_map::Iter<'a, String, String>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
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

    pub fn equal(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self::new(name, value, MatcherOperator::Equal)
    }

    pub fn matches(&self, label_value: &str) -> bool {
        match self.operator {
            MatcherOperator::Equal => self.value == label_value,
            MatcherOperator::NotEqual => self.value != label_value,
        }
    }

    pub fn candidates<'a>(&self, lookup: &'a impl PostingLookup) -> Cow<'a, [SeriesId]> {
        match self.operator {
            MatcherOperator::Equal => lookup
                .values_for(&self.name)
                .and_then(|v| v.get(&self.value))
                .map(|ids| Cow::Borrowed(ids.as_slice()))
                .unwrap_or(Cow::Owned(Vec::new())),

            MatcherOperator::NotEqual => {
                let values = lookup.values_for(&self.name);
                let with_label: Vec<SeriesId> = values.map(union_all).unwrap_or_default();
                let with_this_value: &[SeriesId] = values
                    .and_then(|v| v.get(&self.value))
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);

                let mut result = difference_sorted(&with_label, with_this_value);

                if !self.value.is_empty() {
                    let without_label = difference_sorted(lookup.universe(), &with_label);
                    result = union_sorted(&result, &without_label);
                }
                Cow::Owned(result)
            }
        }
    }
}

pub fn union_sorted(a: &[SeriesId], b: &[SeriesId]) -> Vec<SeriesId> {
    let mut result = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].0.cmp(&b[j].0) {
            std::cmp::Ordering::Less => {
                result.push(a[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                result.push(b[j]);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                result.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    result.extend_from_slice(&a[i..]);
    result.extend_from_slice(&b[j..]);
    result
}

pub fn difference_sorted(a: &[SeriesId], b: &[SeriesId]) -> Vec<SeriesId> {
    let mut result = Vec::with_capacity(a.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].0.cmp(&b[j].0) {
            std::cmp::Ordering::Less => {
                result.push(a[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
    }
    result.extend_from_slice(&a[i..]);
    result
}

pub fn intersect_in_place(a: &mut Vec<SeriesId>, b: &[SeriesId]) {
    let (mut write, mut i, mut j) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].0.cmp(&b[j].0) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                a[write] = a[i];
                write += 1;
                i += 1;
                j += 1;
            }
        }
    }
    a.truncate(write);
}

pub fn union_all(values: &std::collections::HashMap<String, Vec<SeriesId>>) -> Vec<SeriesId> {
    values
        .values()
        .fold(Vec::new(), |acc, v| union_sorted(&acc, v))
}

pub trait PostingLookup {
    fn values_for(&self, name: &str) -> Option<&HashMap<String, Vec<SeriesId>>>;
    fn universe(&self) -> &[SeriesId];
}

pub trait SampleStore {
    /// Appends a sample to the series identified by the given series ID.
    fn append(&mut self, id: SeriesId, sample: Sample) -> Result<(), StorageError>;

    /// Reads samples from the series identified by the given series ID within the specified time range.
    fn read(&self, id: SeriesId, range: TimeRange) -> Result<Vec<Sample>, StorageError>;
}

pub trait SeriesIndex {
    /// Encodes a label set and returns a unique series ID for it.
    /// If the label set has already been encoded, it returns the existing series ID.
    fn encode(&mut self, labels: &LabelSet) -> SeriesId;

    /// Resolves a label set to a list of series IDs that match the label set.
    fn resolve(&self, matchers: &[Matcher]) -> Vec<SeriesId>;

    /// Returns the label set for a given series ID, if it exists.
    fn labels_for(&self, id: SeriesId) -> Option<LabelSet>;

    fn including_label(&self, label_name: &str) -> Vec<SeriesId>;
}

/// TODO: dunno if parts below shouldnt be defined in tsdb-api, cuz its not neccessarily domain
#[derive(Debug, Clone, PartialEq)]
pub struct WriteBatch {
    pub series: Vec<(LabelSet, Vec<Sample>)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeriesResult {
    pub labels: LabelSet,
    pub samples: Vec<Sample>,
}

/// database facade
pub trait Database: Send + Sync {
    /// Writes a batch of series data to the database.
    fn write(&self, batch: WriteBatch) -> Result<(), DbError>;

    /// Queries the database for series that match the given label sets and time range.
    fn query(&self, matchers: &[Matcher], range: TimeRange) -> Result<Vec<SeriesResult>, DbError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label_set(pairs: &[(&str, &str)]) -> LabelSet {
        let mut set = LabelSet::new();
        for (name, value) in pairs {
            set.insert(*name, *value);
        }
        set
    }

    #[test]
    fn new_is_empty() {
        let set = LabelSet::new();
        assert!(set.is_empty());
        assert_eq!(set.len(), 0);
    }

    #[test]
    fn insert_adds_label() {
        let mut set = LabelSet::new();
        set.insert("host", "a");

        assert_eq!(set.get("host"), Some("a"));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn insert_overwrites_existing_value_for_same_name() {
        let mut set = LabelSet::new();
        set.insert("host", "a");
        set.insert("host", "b");

        assert_eq!(set.get("host"), Some("b"));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn remove_deletes_label_and_returns_old_value() {
        let mut set = label_set(&[("host", "a"), ("zone", "eu")]);

        let removed = set.remove("host");

        assert_eq!(removed, Some("a".to_string()));
        assert_eq!(set.get("host"), None);
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn remove_missing_label_returns_none() {
        let mut set = label_set(&[("host", "a")]);
        assert_eq!(set.remove("zone"), None);
    }

    #[test]
    fn equality_is_independent_of_insertion_order() {
        let mut set_a = LabelSet::new();
        set_a.insert("zone", "eu");
        set_a.insert("__name__", "http_requests");
        set_a.insert("method", "get");

        let mut set_b = LabelSet::new();
        set_b.insert("method", "get");
        set_b.insert("zone", "eu");
        set_b.insert("__name__", "http_requests");

        assert_eq!(set_a, set_b);
    }

    #[test]
    fn iteration_is_sorted_by_name_regardless_of_insertion_order() {
        let set = label_set(&[
            ("zone", "eu"),
            ("__name__", "http_requests"),
            ("method", "get"),
        ]);

        let names: Vec<&str> = set.into_iter().map(|(name, _)| name.as_str()).collect();

        assert_eq!(names, vec!["__name__", "method", "zone"]);
    }

    #[test]
    fn metric_name_returns_name_label_value() {
        let set = label_set(&[("method", "get"), ("__name__", "http_requests")]);
        assert_eq!(set.metric_name(), Some("http_requests"));
    }

    #[test]
    fn metric_name_absent_returns_none() {
        let set = label_set(&[("method", "get")]);
        assert_eq!(set.metric_name(), None);
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
        let via_labels = LabelSet::from_labels([
            Label::new("__name__", "http_requests"),
            Label::new("method", "get"),
        ]);
        let via_insert = label_set(&[("__name__", "http_requests"), ("method", "get")]);

        assert_eq!(via_labels, via_insert);
    }

    #[test]
    fn from_labels_later_duplicate_overwrites_earlier() {
        let set = LabelSet::from_labels([Label::new("host", "a"), Label::new("host", "b")]);

        assert_eq!(set.get("host"), Some("b"));
        assert_eq!(set.len(), 1);
    }
}
