use std::sync::Arc;

use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use tsdb::model::{Label, LabelSet, Matcher, MatcherOperator};

const QUERY_BANK_SIZE: usize = 4_096;
pub(crate) const SHARD_VALUES: usize = 128;

pub(crate) const METRICS: &[&str] = &[
    "http_requests_total",
    "cpu_seconds_total",
    "mem_bytes",
    "disk_io",
    "net_bytes",
];

pub(crate) const REGIONS: &[&str] = &[
    "us-east-1",
    "us-west-2",
    "eu-west-1",
    "eu-central-1",
    "ap-south-1",
    "ap-ne-1",
];

pub(crate) const ENVS: &[&str] = &["prod", "staging", "dev"];

const SERVICES: u64 = 40;
const AZS: u64 = 24;
const VERSIONS: u64 = 8;

pub(crate) fn seeded_series_labels(i: u64) -> LabelSet {
    LabelSet::from_labels([
        Label::new("__name__", METRICS[(i % METRICS.len() as u64) as usize]),
        Label::new("region", REGIONS[(i % REGIONS.len() as u64) as usize]),
        Label::new("env", ENVS[((i / 6) % ENVS.len() as u64) as usize]),
        Label::new("az", format!("az-{}", i % AZS)),
        Label::new("service", format!("svc-{}", (i / 3) % SERVICES)),
        Label::new("version", format!("v{}", (i / 7) % VERSIONS)),
        Label::new("job", format!("job-{}", i % 10)),
        Label::new("shard", (i % SHARD_VALUES as u64).to_string()),
        Label::new("instance", format!("inst-{i}")),
    ])
}

pub(crate) fn cold_series_labels(writer: usize, n: u64) -> LabelSet {
    LabelSet::from_labels([
        Label::new("__name__", "load"),
        Label::new("region", REGIONS[(n % REGIONS.len() as u64) as usize]),
        Label::new("env", ENVS[(n % ENVS.len() as u64) as usize]),
        Label::new("shard", (n % SHARD_VALUES as u64).to_string()),
        Label::new("service", format!("svc-{}", n % SERVICES)),
        Label::new("writer", writer.to_string()),
        Label::new("series", n.to_string()),
    ])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shape {
    Point,
    Conj,
    Neg,
    Selective,
    Skewed,
    NegScan,
}

impl Shape {
    pub(crate) const ALL: [Self; 6] = [
        Self::Point,
        Self::Conj,
        Self::Neg,
        Self::Selective,
        Self::Skewed,
        Self::NegScan,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Point => "point",
            Self::Conj => "conjunction",
            Self::Neg => "negative",
            Self::Selective => "selective",
            Self::Skewed => "skewed",
            Self::NegScan => "negative_scan",
        }
    }
}

pub(crate) fn build_query_bank(shape: Shape, series: usize) -> Arc<Vec<Vec<Matcher>>> {
    assert!(series > 0);

    let mut random = SmallRng::seed_from_u64(0x5151_5151 ^ shape as u64);
    let mut queries = Vec::with_capacity(QUERY_BANK_SIZE);

    for _ in 0..QUERY_BANK_SIZE {
        let i = random.random_range(0..series as u64);

        let region = REGIONS[(i % REGIONS.len() as u64) as usize];
        let env_index = ((i / 6) % ENVS.len() as u64) as usize;
        let env = ENVS[env_index];
        let other_env = ENVS[(env_index + 1) % ENVS.len()];

        let shard = (i % SHARD_VALUES as u64).to_string();
        let service = format!("svc-{}", (i / 3) % SERVICES);
        let instance = format!("inst-{i}");

        let matchers = match shape {
            Shape::Point => vec![Matcher::new("instance", instance, MatcherOperator::Equal)],
            Shape::Conj => vec![
                Matcher::new("shard", shard, MatcherOperator::Equal),
                Matcher::new("region", region, MatcherOperator::Equal),
            ],
            Shape::Neg => vec![Matcher::new("env", other_env, MatcherOperator::NotEqual)],
            Shape::Selective => vec![
                Matcher::new("service", service, MatcherOperator::Equal),
                Matcher::new("shard", shard, MatcherOperator::Equal),
            ],
            Shape::Skewed => vec![
                Matcher::new("env", env, MatcherOperator::Equal),
                Matcher::new("instance", instance, MatcherOperator::Equal),
            ],
            Shape::NegScan => vec![
                Matcher::new("service", service, MatcherOperator::Equal),
                Matcher::new("shard", shard, MatcherOperator::Equal),
                Matcher::new("env", other_env, MatcherOperator::NotEqual),
            ],
        };

        queries.push(matchers);
    }

    Arc::new(queries)
}
