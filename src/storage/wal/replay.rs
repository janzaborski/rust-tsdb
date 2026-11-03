use super::record::{SamplesEntry, SeriesEntry, WalRecord};
use crate::model::{LabelSet, Sample, SeriesId};
use std::collections::HashMap;

pub trait ReplaySink {
    fn series(&mut self, labels: LabelSet) -> SeriesId;
    fn samples(&mut self, id: SeriesId, samples: Vec<Sample>);
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ResolveStats {
    pub late_series: u64,
    pub unknown_series: u64,
    pub dropped_samples: u64,
}

#[derive(Default)]
pub(crate) struct Resolver {
    ids: HashMap<SeriesId, SeriesId>,
    pending: HashMap<SeriesId, Vec<Vec<Sample>>>,
    stats: ResolveStats,
}

impl Resolver {
    pub(crate) fn apply(&mut self, record: WalRecord, sink: &mut impl ReplaySink) {
        for SeriesEntry { id, labels } in record.series {
            let sink_id = sink.series(labels);
            self.ids.insert(id, sink_id);
            if let Some(parked) = self.pending.remove(&id) {
                self.stats.late_series += 1;
                for samples in parked {
                    sink.samples(sink_id, samples);
                }
            }
        }
        for SamplesEntry { id, samples } in record.samples {
            match self.ids.get(&id) {
                Some(&sink_id) => sink.samples(sink_id, samples),
                None => self.pending.entry(id).or_default().push(samples),
            }
        }
    }

    pub(crate) fn finish(mut self) -> ResolveStats {
        self.stats.unknown_series = self.pending.len() as u64;
        self.stats.dropped_samples = self
            .pending
            .values()
            .flatten()
            .map(|s| s.len() as u64)
            .sum();
        self.stats
    }
}
