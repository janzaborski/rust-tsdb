use super::error::{DbError, StorageError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelSet(Vec<Label>);

impl LabelSet {
    /// Normalizes the label set by sorting the labels by name.
    pub fn normalize(&mut self) {
        self.0.sort_by(|a, b| a.name.cmp(&b.name));
    }

    /// Returns metric name if it exists (that is label named __name__)
    pub fn metric_name(&self) -> Option<&str> {
        self.0
            .iter()
            .find(|label| label.name == "__name__")
            .map(|label| label.value.as_str())
    }

    /// Returns the value of the label with the given name, if it exists.   
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|label| label.name == name)
            .map(|label| label.value.as_str())
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

impl From<(u64, f64)> for Sample {
    fn from((timestamp, value): (u64, f64)) -> Self {
        Self { timestamp, value }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// For now same shit as Label, but will become helpful when we implement matchers and operators
pub struct Matcher {
    pub name: String,
    pub value: String,
    // pub operator: MatcherOperator,
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
    fn encode(&self, labels: &LabelSet) -> SeriesId;

    /// Resolves a label set to a list of series IDs that match the label set.
    fn resolve(&self, matchers: &[Matcher]) -> Vec<SeriesId>;

    /// Returns the label set for a given series ID, if it exists.
    fn labels_for(&self, id: SeriesId) -> Option<LabelSet>;
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

    fn label(name: &str, value: &str) -> Label {
        Label {
            name: name.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn normalize_sorts_labels_by_name() {
        let mut set = LabelSet(vec![
            label("zone", "eu"),
            label("__name__", "http_requests"),
            label("method", "get"),
        ]);
        set.normalize();

        let names: Vec<&str> = set.0.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["__name__", "method", "zone"]);
    }

    #[test]
    fn normalize_is_stable_for_already_sorted_set() {
        let mut set = LabelSet(vec![label("a", "1"), label("b", "2"), label("c", "3")]);
        let expected = set.clone();
        set.normalize();
        assert_eq!(set, expected);
    }

    #[test]
    fn normalize_on_empty_set_is_noop() {
        let mut set = LabelSet(vec![]);
        set.normalize();
        assert_eq!(set, LabelSet(vec![]));
    }

    #[test]
    fn metric_name_returns_name_label_value() {
        let set = LabelSet(vec![
            label("method", "get"),
            label("__name__", "http_requests"),
        ]);
        assert_eq!(set.metric_name(), Some("http_requests"));
    }

    #[test]
    fn metric_name_absent_returns_none() {
        let set = LabelSet(vec![label("method", "get")]);
        assert_eq!(set.metric_name(), None);
    }

    #[test]
    fn get_returns_value_for_existing_label() {
        let set = LabelSet(vec![label("method", "get"), label("zone", "eu")]);
        assert_eq!(set.get("zone"), Some("eu"));
    }

    #[test]
    fn get_returns_none_for_missing_label() {
        let set = LabelSet(vec![label("method", "get")]);
        assert_eq!(set.get("zone"), None);
    }

    #[test]
    fn get_returns_first_match_when_duplicated() {
        let set = LabelSet(vec![label("env", "prod"), label("env", "staging")]);
        assert_eq!(set.get("env"), Some("prod"));
    }
}
