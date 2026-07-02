use super::SeriesId;
use crate::series::labels::label_set::LabelSet;

pub trait SeriesIndex {
    fn intern(&self, label_set: LabelSet) -> Option<SeriesId>;
    fn resolve(&self, label_set: LabelSet) -> Vec<SeriesId>;
    fn labels_for(&self, series_id: SeriesId) -> Option<LabelSet>;
}
