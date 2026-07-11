use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tsdb_core::{DbError, Label, LabelSet, Matcher, MatcherOperator, Sample, TimeRange};

pub trait Database: Send + Sync {
    fn write(&self, batch: WriteBatch) -> Result<(), DbError>;
    fn query(&self, matchers: &[Matcher], range: TimeRange) -> Result<Vec<SeriesResult>, DbError>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct WriteBatch {
    pub series: Vec<(LabelSet, Vec<Sample>)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeriesResult {
    pub labels: LabelSet,
    pub samples: Vec<Sample>,
}

pub fn router(db: Arc<dyn Database>) -> Router {
    Router::new()
        .route("/api/v1/write_json", post(write_json))
        .route("/api/v1/read", get(read))
        .with_state(db)
}

#[derive(Deserialize)]
struct WriteSeries {
    labels: HashMap<String, String>,
    samples: Vec<SampleDto>,
}

#[derive(Deserialize)]
struct SampleDto {
    t: u64,
    v: f64,
}

async fn write_json(
    State(db): State<Arc<dyn Database>>,
    Json(body): Json<Vec<WriteSeries>>,
) -> impl IntoResponse {
    let batch = WriteBatch {
        series: body
            .into_iter()
            .map(|s| {
                let labels =
                    LabelSet::from_labels(s.labels.into_iter().map(|(n, v)| Label::new(n, v)));
                let samples = s
                    .samples
                    .into_iter()
                    .map(|d| Sample::new(d.t, d.v))
                    .collect();
                (labels, samples)
            })
            .collect(),
    };

    match db.write(batch) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn read(
    State(db): State<Arc<dyn Database>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let mut start = 0u64;
    let mut end = u64::MAX;
    let mut matchers = Vec::new();
    for (k, v) in params {
        match k.as_str() {
            "start" => start = v.parse().unwrap_or(0),
            "end" => end = v.parse().unwrap_or(u64::MAX),
            "name" => matchers.push(Matcher::new("__name__", v, MatcherOperator::Equal)),
            _ => matchers.push(Matcher::new(k, v, MatcherOperator::Equal)),
        }
    }

    match db.query(&matchers, TimeRange::new(start, end)) {
        Ok(results) => Json(to_matrix(results)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

fn to_matrix(results: Vec<SeriesResult>) -> Value {
    let result: Vec<Value> = results
        .into_iter()
        .map(|s| {
            let metric: serde_json::Map<String, Value> = (&s.labels)
                .into_iter()
                .map(|(n, v)| (n.clone(), Value::String(v.clone())))
                .collect();
            let values: Vec<Value> = s
                .samples
                .iter()
                // Prometheus matrix: [ <seconds as float>, "<value as string>" ]
                .map(|smp| json!([smp.timestamp as f64 / 1000.0, smp.value.to_string()]))
                .collect();
            json!({ "metric": metric, "values": values })
        })
        .collect();

    json!({
        "status": "success",
        "data": { "resultType": "matrix", "result": result }
    })
}
