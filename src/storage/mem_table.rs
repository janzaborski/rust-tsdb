use crate::model::{Sample, SeriesId, TimeRange};
use crate::storage::{SampleStore, StorageError};
use std::collections::HashMap;

#[derive(Default)]
pub struct MemTable {
    pub data: HashMap<SeriesId, Vec<Sample>>,
}

impl MemTable {
    pub fn new() -> MemTable {
        Self::default()
    }
}

impl SampleStore for MemTable {
    fn append(&mut self, id: SeriesId, sample: Sample) -> Result<(), StorageError> {
        self.data.entry(id).or_default().push(sample);
        Ok(())
    }

    fn read(&self, id: SeriesId, range: TimeRange) -> Result<Vec<Sample>, StorageError> {
        let series = self.data.get(&id).ok_or(StorageError::ReadSamples(format!(
            "no Series with Id: {id:?}"
        )))?;

        Ok(series
            .iter()
            .filter(|s| s.in_timerange(range))
            .copied()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_creates_new_series() {
        let mut store = MemTable::new();
        let id = SeriesId(1);

        store.append(id, Sample::new(100, 1.0)).unwrap();

        assert_eq!(store.data.get(&id).unwrap().len(), 1);
    }

    #[test]
    fn append_pushes_to_existing_series() {
        let mut store = MemTable::new();
        let id = SeriesId(1);

        store.append(id, Sample::new(100, 1.0)).unwrap();
        store.append(id, Sample::new(200, 2.0)).unwrap();

        let series = store.data.get(&id).unwrap();
        assert_eq!(series.len(), 2);
        assert_eq!(series[0], Sample::new(100, 1.0));
        assert_eq!(series[1], Sample::new(200, 2.0));
    }

    #[test]
    fn append_keeps_series_separate() {
        let mut store = MemTable::new();
        let id_a = SeriesId(1);
        let id_b = SeriesId(2);

        store.append(id_a, Sample::new(100, 1.0)).unwrap();
        store.append(id_b, Sample::new(100, 2.0)).unwrap();

        assert_eq!(store.data.get(&id_a).unwrap().len(), 1);
        assert_eq!(store.data.get(&id_b).unwrap().len(), 1);
    }

    #[test]
    fn read_returns_error_for_unknown_series() {
        let store = MemTable::new();
        let id = SeriesId(1);
        let range = TimeRange::new(0, 1000);

        let result = store.read(id, range);

        assert!(result.is_err());
    }

    #[test]
    fn read_filters_by_range_inclusive() {
        let mut store = MemTable::new();
        let id = SeriesId(1);

        store.append(id, Sample::new(100, 1.0)).unwrap();
        store.append(id, Sample::new(200, 2.0)).unwrap();
        store.append(id, Sample::new(300, 3.0)).unwrap();

        let range = TimeRange::new(100, 200);
        let result = store.read(id, range).unwrap();

        assert_eq!(result, vec![Sample::new(100, 1.0), Sample::new(200, 2.0)]);
    }

    #[test]
    fn read_returns_empty_when_no_samples_in_range() {
        let mut store = MemTable::new();
        let id = SeriesId(1);

        store.append(id, Sample::new(100, 1.0)).unwrap();

        let range = TimeRange::new(500, 600);
        let result = store.read(id, range).unwrap();

        assert!(result.is_empty());
    }

    #[test]
    fn read_does_not_leak_other_series() {
        let mut store = MemTable::new();
        let id_a = SeriesId(1);
        let id_b = SeriesId(2);

        store.append(id_a, Sample::new(100, 1.0)).unwrap();
        store.append(id_b, Sample::new(100, 2.0)).unwrap();

        let range = TimeRange::new(0, 1000);
        let result = store.read(id_a, range).unwrap();

        assert_eq!(result, vec![Sample::new(100, 1.0)]);
    }
}
