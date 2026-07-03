// TODO:
// 1. Proper error handling with custom error types instead of String
// 2. Think about how to handle empty LabelSets
// 3. Write Unit tests with 100% coverage

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

#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub timestamp_ms: u64,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeRange {
    pub start_ms: u64,
    pub end_ms: u64,
}

/// For now same shit as LabelSet, but will become helpful when we implement matchers and operators
pub struct Matcher {
    pub name: String,
    pub value: String,
    // pub operator: MatcherOperator,
}

pub trait SampleStore {
    /// Appends a sample to the series identified by the given series ID.
    fn append(&self, id: SeriesId, sample: Sample) -> Result<(), String>;

    /// Reads samples from the series identified by the given series ID within the specified time range.
    fn read(&self, id: SeriesId, range: TimeRange) -> Result<Vec<Sample>, String>;
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
    fn write(&self, batch: WriteBatch) -> Result<(), String>;

    /// Queries the database for series that match the given label sets and time range.
    fn query(&self, matchers: &[Matcher], range: TimeRange) -> Result<Vec<SeriesResult>, String>;
}
