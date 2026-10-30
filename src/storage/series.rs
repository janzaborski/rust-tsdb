use std::sync::{Arc, Mutex, MutexGuard};

use crate::model::{LabelSet, Sample, SeriesId, TimeRange};

#[derive(Debug)]
pub struct Series {
    pub id: SeriesId,
    pub labels: Arc<LabelSet>,
    state: Mutex<SeriesState>,
}

#[derive(Clone, Debug, Default)]
pub struct SeriesState {
    samples: Vec<Sample>,
    newest: Option<u64>,
    logged_gen: u64,
}

impl Series {
    pub fn new(id: SeriesId, labels: Arc<LabelSet>) -> Self {
        Self {
            id,
            labels,
            state: Mutex::default(),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, SeriesState> {
        self.state.lock().unwrap()
    }
}

impl SeriesState {
    pub fn accepts(&self, timestamp: u64) -> bool {
        self.newest.is_none_or(|n| timestamp > n)
    }

    pub fn append(&mut self, s: Sample) -> bool {
        let ok = self.accepts(s.timestamp);

        if ok {
            self.samples.push(s);
            self.newest = Some(s.timestamp);
        }

        ok
    }

    pub fn newest(&self) -> Option<u64> {
        self.newest
    }

    pub fn samples(&self) -> &[Sample] {
        &self.samples
    }

    pub fn range(&self, range: TimeRange) -> &[Sample] {
        let lo = self.samples.partition_point(|s| s.timestamp < range.start);
        let hi = self.samples.partition_point(|s| s.timestamp <= range.end);

        &self.samples[lo..hi.max(lo)]
    }

    pub fn is_logged(&self, generation: u64) -> bool {
        self.logged_gen == generation
    }

    pub fn mark_logged(&mut self, generation: u64) {
        self.logged_gen = generation;
    }

    pub fn take_samples(&mut self) -> Vec<Sample> {
        std::mem::take(&mut self.samples)
    }

    pub fn restore_newest(&mut self, newest: u64) {
        match self.newest {
            Some(n) => self.newest = Some(n.max(newest)),
            None => self.newest = Some(newest),
        };
    }
}
