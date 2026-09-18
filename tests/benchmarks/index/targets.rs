use parking_lot::RwLock;
use tsdb::model::{LabelSet, Matcher, SeriesId};
use tsdb::packed_model::PackedLabelSet;
use tsdb::storage::Index as StorageIndex;
use tsdb::storage::indexes::index_v10a::IndexV10A;
use tsdb::storage::indexes::index_v10a_packed::IndexV10APacked;
use tsdb::storage::indexes::index_v11a::IndexV11A;
use tsdb::storage::indexes::index_v11a_packed::IndexV11APacked;
use tsdb::storage::indexes::simple_index::SimpleIndex;

pub(crate) trait BenchIndex: Send + Sync + Sized + 'static {
    type Labels: Send + Sync + 'static;

    fn new() -> Self;
    fn native(labels: LabelSet) -> Self::Labels;
    fn encode(&self, labels: &Self::Labels) -> SeriesId;
    fn lookup(&self, labels: &Self::Labels) -> Option<SeriesId>;
    fn resolve(&self, matchers: &[Matcher]) -> Vec<SeriesId>;

    fn inspect_labels(&self, id: SeriesId) -> Option<usize>;

    fn label_strings(&self, id: SeriesId) -> Option<Vec<(String, String)>>;
}

fn standard_strings(labels: &LabelSet) -> Vec<(String, String)> {
    labels
        .iter()
        .map(|label| (label.name.clone(), label.value.clone()))
        .collect()
}

fn packed_strings(labels: &PackedLabelSet) -> Vec<(String, String)> {
    labels
        .iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

macro_rules! locked_standard {
    ($adapter:ident, $index:ty) => {
        pub(crate) struct $adapter(RwLock<$index>);

        impl BenchIndex for $adapter {
            type Labels = LabelSet;

            fn new() -> Self {
                Self(RwLock::new(<$index>::new()))
            }

            fn native(labels: LabelSet) -> Self::Labels {
                labels
            }

            fn encode(&self, labels: &Self::Labels) -> SeriesId {
                if let Some(id) = self.0.read().lookup(labels) {
                    id
                } else {
                    self.0.write().encode(labels)
                }
            }

            fn lookup(&self, labels: &Self::Labels) -> Option<SeriesId> {
                self.0.read().lookup(labels)
            }

            fn resolve(&self, matchers: &[Matcher]) -> Vec<SeriesId> {
                self.0.read().resolve(matchers)
            }

            fn inspect_labels(&self, id: SeriesId) -> Option<usize> {
                self.0.read().labels_for(id).map(|labels| labels.len())
            }

            fn label_strings(&self, id: SeriesId) -> Option<Vec<(String, String)>> {
                self.0
                    .read()
                    .labels_for(id)
                    .map(|labels| standard_strings(&labels))
            }
        }
    };
}

locked_standard!(Simple, SimpleIndex);
locked_standard!(V11A, IndexV11A);

macro_rules! locked_packed {
    ($adapter:ident, $index:ty) => {
        pub(crate) struct $adapter(RwLock<$index>);

        impl BenchIndex for $adapter {
            type Labels = PackedLabelSet;

            fn new() -> Self {
                Self(RwLock::new(<$index>::new()))
            }

            fn native(labels: LabelSet) -> Self::Labels {
                PackedLabelSet::from_label_set(&labels)
            }

            fn encode(&self, labels: &Self::Labels) -> SeriesId {
                if let Some(id) = self.0.read().lookup(labels) {
                    id
                } else {
                    self.0.write().encode(labels)
                }
            }

            fn lookup(&self, labels: &Self::Labels) -> Option<SeriesId> {
                self.0.read().lookup(labels)
            }

            fn resolve(&self, matchers: &[Matcher]) -> Vec<SeriesId> {
                self.0.read().resolve(matchers)
            }

            fn inspect_labels(&self, id: SeriesId) -> Option<usize> {
                self.0.read().labels_for(id).map(|labels| labels.len())
            }

            fn label_strings(&self, id: SeriesId) -> Option<Vec<(String, String)>> {
                self.0
                    .read()
                    .labels_for(id)
                    .map(|labels| packed_strings(&labels))
            }
        }
    };
}
locked_packed!(V11APacked, IndexV11APacked);

macro_rules! concurrent {
    ($adapter:ident, $index:ty, $labels:ty, $convert:expr, $strings:ident) => {
        pub(crate) struct $adapter($index);

        impl BenchIndex for $adapter {
            type Labels = $labels;

            fn new() -> Self {
                Self(<$index>::new())
            }

            fn native(labels: LabelSet) -> Self::Labels {
                $convert(labels)
            }

            fn encode(&self, labels: &Self::Labels) -> SeriesId {
                self.0.encode(labels)
            }

            fn lookup(&self, labels: &Self::Labels) -> Option<SeriesId> {
                self.0.lookup(labels)
            }

            fn resolve(&self, matchers: &[Matcher]) -> Vec<SeriesId> {
                self.0.resolve(matchers)
            }

            fn inspect_labels(&self, id: SeriesId) -> Option<usize> {
                self.0.labels_for(id).map(|labels| labels.len())
            }

            fn label_strings(&self, id: SeriesId) -> Option<Vec<(String, String)>> {
                self.0.labels_for(id).map(|labels| $strings(&labels))
            }
        }
    };
}

concurrent!(
    Index,
    StorageIndex,
    LabelSet,
    |labels| labels,
    standard_strings
);
concurrent!(V10A, IndexV10A, LabelSet, |labels| labels, standard_strings);
concurrent!(
    V10APacked,
    IndexV10APacked,
    PackedLabelSet,
    |labels: LabelSet| PackedLabelSet::from_label_set(&labels),
    packed_strings
);

#[derive(Debug, Clone, Copy)]
pub(crate) enum Target {
    Index,
    Simple,
    V10A,
    V10APacked,
    V11A,
    V11APacked,
}

impl Target {
    pub(crate) const ALL: [Self; 6] = [
        Self::Index,
        Self::Simple,
        Self::V10A,
        Self::V10APacked,
        Self::V11A,
        Self::V11APacked,
    ];

    pub(crate) const PERFORMANCE: [Self; 6] = Self::ALL;

    pub(crate) const WORKLOAD: [Self; 6] = Self::PERFORMANCE;

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Index => "index",
            Self::Simple => "simple",
            Self::V10A => "v10a",
            Self::V10APacked => "v10a_packed",
            Self::V11A => "v11a",
            Self::V11APacked => "v11a_packed",
        }
    }
}
