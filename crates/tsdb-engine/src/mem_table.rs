use std::collections::HashMap;
use tsdb_core::{Sample, SampleStore, SeriesId, StorageError, TimeRange};

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
    fn create_new_series(&mut self, id: SeriesId) -> Result<(), StorageError> {
        match self.data.entry(id) {
            std::collections::hash_map::Entry::Occupied(_) => Err(StorageError::CreateNewSeries(
                format!("Sereis with Id: {id:?} already exists"),
            )),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(Vec::new());
                Ok(())
            }
        }
    }

    fn append(&mut self, id: SeriesId, sample: Sample) -> Result<(), StorageError> {
        let series = self
            .data
            .get_mut(&id)
            .ok_or_else(|| StorageError::AppendSample(format!("no Series with id: {id:?}")))?;

        series.push(sample);
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
    fn create_new_series_succeeds_for_new_id() {
        let mut store = MemTable::new();
        let id = SeriesId(1);

        let result = store.create_new_series(id);

        assert!(result.is_ok());
        assert!(store.data.get(&id).unwrap().is_empty());
    }

    #[test]
    fn create_new_series_fails_if_already_exists() {
        let mut store = MemTable::new();
        let id = SeriesId(1);

        store.create_new_series(id).unwrap();
        let result = store.create_new_series(id);

        assert!(result.is_err());
    }

    #[test]
    fn create_new_series_does_not_overwrite_existing_data() {
        let mut store = MemTable::new();
        let id = SeriesId(1);

        store.create_new_series(id).unwrap();
        store.append(id, Sample::new(100, 1.0)).unwrap();

        let result = store.create_new_series(id);

        assert!(result.is_err());
        assert_eq!(store.data.get(&id).unwrap().len(), 1);
    }

    #[test]
    fn append_fails_if_series_not_created() {
        let mut store = MemTable::new();
        let id = SeriesId(1);

        let result = store.append(id, Sample::new(100, 1.0));

        assert!(result.is_err());
    }

    #[test]
    fn append_succeeds_after_series_created() {
        let mut store = MemTable::new();
        let id = SeriesId(1);

        store.create_new_series(id).unwrap();
        store.append(id, Sample::new(100, 1.0)).unwrap();

        assert_eq!(store.data.get(&id).unwrap().len(), 1);
    }

    #[test]
    fn append_pushes_to_existing_series() {
        let mut store = MemTable::new();
        let id = SeriesId(1);

        store.create_new_series(id).unwrap();
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

        store.create_new_series(id_a).unwrap();
        store.create_new_series(id_b).unwrap();
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

        store.create_new_series(id).unwrap();
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

        store.create_new_series(id).unwrap();
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

        store.create_new_series(id_a).unwrap();
        store.create_new_series(id_b).unwrap();
        store.append(id_a, Sample::new(100, 1.0)).unwrap();
        store.append(id_b, Sample::new(100, 2.0)).unwrap();

        let range = TimeRange::new(0, 1000);
        let result = store.read(id_a, range).unwrap();

        assert_eq!(result, vec![Sample::new(100, 1.0)]);
    }
}
