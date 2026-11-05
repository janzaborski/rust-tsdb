mod decode;

use std::collections::HashMap;
use std::sync::Arc;

use crate::db::{Db, DbError, SeriesResult, WriteBatch};
use crate::model::{LabelSet, Matcher, MatcherOperator, Sample, TimeRange};
use crate::storage::wal::WalError;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn router(db: Arc<Db>) -> Router {
    Router::new()
        .route("/api/v1/write", post(remote_write))
        .route("/debug/write_json", post(write_json))
        .route("/debug/read", get(read))
        .layer(DefaultBodyLimit::max(decode::MAX_COMPRESSED))
        .with_state(db)
}

async fn remote_write(State(db): State<Arc<Db>>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(e) = check_remote_headers(&headers) {
        return e.into_response();
    }
    match decode::decompress(&body).and_then(|raw| decode::decode_write_request(&raw)) {
        Ok(series) => write(&db, WriteBatch { series }).await,
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

fn check_remote_headers(headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    let value = |name| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    };
    let content_type = value(header::CONTENT_TYPE);
    let supported = content_type.starts_with("application/x-protobuf")
        && content_type
            .split(';')
            .filter_map(|p| p.trim().strip_prefix("proto="))
            .all(|p| p == "prometheus.WriteRequest");
    if !supported {
        let msg = format!("unsupported content type {content_type:?}");
        return Err((StatusCode::UNSUPPORTED_MEDIA_TYPE, msg));
    }
    if !value(header::CONTENT_ENCODING).eq_ignore_ascii_case("snappy") {
        return Err((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "expected Content-Encoding: snappy".into(),
        ));
    }
    Ok(())
}

async fn write(db: &Db, batch: WriteBatch) -> Response {
    match db.write(batch) {
        Ok(lsn) => {
            db.wait(lsn).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => (status(&e), e.to_string()).into_response(),
    }
}

fn status(e: &DbError) -> StatusCode {
    match e {
        DbError::InvalidTimeRange { .. } | DbError::InvalidWriteBatch(_) => StatusCode::BAD_REQUEST,
        DbError::Wal(WalError::RecordTooLarge(_)) => StatusCode::PAYLOAD_TOO_LARGE,
        DbError::Wal(WalError::Closed) => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
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

async fn write_json(State(db): State<Arc<Db>>, Json(body): Json<Vec<WriteSeries>>) -> Response {
    let batch = WriteBatch {
        series: body
            .into_iter()
            .map(|s| {
                let labels =
                    LabelSet::from_pairs(s.labels.iter().map(|(n, v)| (n.as_str(), v.as_str())));
                let samples = s
                    .samples
                    .into_iter()
                    .map(|d| Sample::new(d.t, d.v))
                    .collect();
                (labels, samples)
            })
            .collect(),
    };
    write(&db, batch).await
}

async fn read(
    State(db): State<Arc<Db>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut start = 0u64;
    let mut end = u64::MAX;
    let mut matchers = Vec::new();
    for (k, v) in params {
        match k.as_str() {
            "start" => {
                start = v.parse().map_err(|_| {
                    (
                        StatusCode::BAD_REQUEST,
                        "start must be an unsigned integer in milliseconds".to_owned(),
                    )
                })?;
            }
            "end" => {
                end = v.parse().map_err(|_| {
                    (
                        StatusCode::BAD_REQUEST,
                        "end must be an unsigned integer in milliseconds".to_owned(),
                    )
                })?;
            }
            "name" => matchers.push(Matcher::new("__name__", v, MatcherOperator::Equal)),
            _ => matchers.push(Matcher::new(k, v, MatcherOperator::Equal)),
        }
    }

    let results = db
        .query(&matchers, TimeRange::new(start, end))
        .map_err(|e| (status(&e), e.to_string()))?;
    Ok(Json(to_matrix(results)))
}

fn to_matrix(results: Vec<SeriesResult>) -> Value {
    let result: Vec<Value> = results
        .into_iter()
        .map(|s| {
            let metric: serde_json::Map<String, Value> = (&s.labels)
                .into_iter()
                .map(|(n, v)| (n.to_owned(), Value::String(v.to_owned())))
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
