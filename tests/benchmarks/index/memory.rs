use std::hint::black_box;

use tsdb::model::LabelSet;
use tsdb::packed_model::PackedLabelSet;
use tsdb::storage::Index;
use tsdb::storage::indexes::index_v10a::IndexV10A;
use tsdb::storage::indexes::index_v10a_packed::IndexV10APacked;
use tsdb::storage::indexes::index_v11a::IndexV11A;
use tsdb::storage::indexes::index_v11a_packed::IndexV11APacked;
use tsdb::storage::indexes::simple_index::SimpleIndex;

use super::shared::seeded_series_labels;

const SERIES_COUNTS: &[usize] = &[100_000, 500_000];

fn selected(name: &str) -> bool {
    std::env::var("TSDB_MEMORY_TARGET").is_ok_and(|value| value != name)
}

fn report<T, I>(name: &str, series: usize, labels: Vec<T>, build: impl FnOnce(&[T]) -> I) {
    let before = crate::live_bytes();
    let index = build(&labels);
    let bytes = crate::live_bytes() - before;
    black_box(&index);
    println!(
        "memory target={name} series={series} index_bytes={bytes} bytes_per_series={:.2}",
        bytes as f64 / series as f64,
    );
    drop(index);
    drop(labels);
}

fn standard_labels(series: usize) -> Vec<LabelSet> {
    (0..series as u64).map(seeded_series_labels).collect()
}

fn packed_labels(series: usize) -> Vec<PackedLabelSet> {
    (0..series as u64)
        .map(seeded_series_labels)
        .map(|labels| PackedLabelSet::from_label_set(&labels))
        .collect()
}

#[test]
#[ignore = "live-allocation index memory benchmark"]
fn bench_index_memory() {
    for &series in SERIES_COUNTS {
        if !selected("index") {
            report("index", series, standard_labels(series), |labels| {
                let index = Index::new();
                for labels in labels {
                    black_box(index.encode(labels));
                }
                index
            });
        }
        if !selected("simple") {
            report("simple", series, standard_labels(series), |labels| {
                let mut index = SimpleIndex::new();
                for labels in labels {
                    black_box(index.encode(labels));
                }
                index
            });
        }
        if !selected("v10a") {
            report("v10a", series, standard_labels(series), |labels| {
                let index = IndexV10A::new();
                for labels in labels {
                    black_box(index.encode(labels));
                }
                index
            });
        }
        if !selected("v10a_packed") {
            report("v10a_packed", series, packed_labels(series), |labels| {
                let index = IndexV10APacked::new();
                for labels in labels {
                    black_box(index.encode(labels));
                }
                index
            });
        }
        if !selected("v11a") {
            report("v11a", series, standard_labels(series), |labels| {
                let mut index = IndexV11A::new();
                for labels in labels {
                    black_box(index.encode(labels));
                }
                index
            });
        }
        if !selected("v11a_packed") {
            report("v11a_packed", series, packed_labels(series), |labels| {
                let mut index = IndexV11APacked::new();
                for labels in labels {
                    black_box(index.encode(labels));
                }
                index
            });
        }
    }
}
