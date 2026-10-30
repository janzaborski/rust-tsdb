use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct LabelSet {
    labels: Vec<Label>,
}

impl LabelSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_labels(labels: impl IntoIterator<Item = Label>) -> Self {
        let mut v: Vec<Label> = labels.into_iter().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v.dedup_by(|a, b| {
            if a.name == b.name {
                std::mem::swap(a, b);
                true
            } else {
                false
            }
        });
        Self::from_sorted_unchecked(v)
    }

    pub fn from_sorted_unchecked(labels: Vec<Label>) -> Self {
        Self { labels }
    }

    /// Returns metric name if it exists (that is label named __name__)
    pub fn metric_name(&self) -> Option<&str> {
        self.get("__name__")
    }

    /// Returns the value of the label with the given name, if it exists.   
    pub fn get(&self, name: &str) -> Option<&str> {
        self.labels
            .binary_search_by(|l| l.name.as_str().cmp(name))
            .ok()
            .map(|i| self.labels[i].value.as_str())
    }

    pub fn insert_label(&mut self, label: Label) {
        match self
            .labels
            .binary_search_by(|existing| existing.name.cmp(&label.name))
        {
            Ok(i) if self.labels[i] == label => {}
            Ok(i) => self.labels[i] = label,
            Err(i) => self.labels.insert(i, label),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }

    pub fn len(&self) -> usize {
        self.labels.len()
    }
}

type PairFn<'a> = fn(&'a Label) -> (&'a String, &'a String);

impl<'a> IntoIterator for &'a LabelSet {
    type Item = (&'a String, &'a String);
    type IntoIter = std::iter::Map<std::slice::Iter<'a, Label>, PairFn<'a>>;

    fn into_iter(self) -> Self::IntoIter {
        self.labels
            .iter()
            .map((|l| (&l.name, &l.value)) as PairFn<'a>)
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
        let mut set = LabelSet::new();
        for (name, value) in pairs {
            set.insert_label(Label::new(*name, *value));
        }
        set
    }

    #[test]
    fn new_is_empty() {
        let set = LabelSet::new();
        assert!(set.is_empty());
    }

    #[test]
    fn insert_adds_label() {
        let mut set = LabelSet::new();
        set.insert_label(Label::new("host", "a"));

        assert_eq!(set.get("host"), Some("a"));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn insert_overwrites_existing_value_for_same_name() {
        let mut set = LabelSet::new();
        set.insert_label(Label::new("host", "a"));
        set.insert_label(Label::new("host", "b"));

        assert_eq!(set.get("host"), Some("b"));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn equality_is_independent_of_insertion_order() {
        let mut set_a = LabelSet::new();
        set_a.insert_label(Label::new("zone", "eu"));
        set_a.insert_label(Label::new("__name__", "http_requests"));
        set_a.insert_label(Label::new("method", "get"));

        let mut set_b = LabelSet::new();
        set_b.insert_label(Label::new("method", "get"));
        set_b.insert_label(Label::new("zone", "eu"));
        set_b.insert_label(Label::new("__name__", "http_requests"));

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
