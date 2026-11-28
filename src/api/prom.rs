use crate::{Db, query};
use axum::{
    Extension, Json,
    extract::{
        Form, Path, Query, State,
        rejection::{FormRejection, QueryRejection},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy)]
pub(super) struct QueryLookback(pub u64);

pub async fn instant_get(
    State(db): State<Arc<Db>>,
    Extension(lookback): Extension<QueryLookback>,
    params: Result<Query<HashMap<String, String>>, QueryRejection>,
) -> Response {
    match params {
        Ok(Query(p)) => run(db, p, true, lookback.0).await,
        Err(e) => error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}
pub async fn instant_post(
    State(db): State<Arc<Db>>,
    Extension(lookback): Extension<QueryLookback>,
    params: Result<Form<HashMap<String, String>>, FormRejection>,
) -> Response {
    match params {
        Ok(Form(p)) => run(db, p, true, lookback.0).await,
        Err(e) => error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}
pub async fn range_get(
    State(db): State<Arc<Db>>,
    Extension(lookback): Extension<QueryLookback>,
    params: Result<Query<HashMap<String, String>>, QueryRejection>,
) -> Response {
    match params {
        Ok(Query(p)) => run(db, p, false, lookback.0).await,
        Err(e) => error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}
pub async fn range_post(
    State(db): State<Arc<Db>>,
    Extension(lookback): Extension<QueryLookback>,
    params: Result<Form<HashMap<String, String>>, FormRejection>,
) -> Response {
    match params {
        Ok(Form(p)) => run(db, p, false, lookback.0).await,
        Err(e) => error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

type CatalogParams = Vec<(String, String)>;

pub async fn labels_get(
    State(db): State<Arc<Db>>,
    params: Result<Query<CatalogParams>, QueryRejection>,
) -> Response {
    match params {
        Ok(Query(params)) => discover(db, None, params).await,
        Err(e) => error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

pub async fn labels_post(
    State(db): State<Arc<Db>>,
    params: Result<Form<CatalogParams>, FormRejection>,
) -> Response {
    match params {
        Ok(Form(params)) => discover(db, None, params).await,
        Err(e) => error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

pub async fn label_values(
    State(db): State<Arc<Db>>,
    Path(name): Path<String>,
    params: Result<Query<CatalogParams>, QueryRejection>,
) -> Response {
    match params {
        Ok(Query(params)) => discover(db, Some(name), params).await,
        Err(e) => error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

pub async fn metadata() -> Json<serde_json::Value> {
    Json(json!({"status":"success","data":{}}))
}

fn discovery_params(params: CatalogParams) -> Result<(Vec<query::Selector>, usize), String> {
    let (mut start, mut end) = (None, None);
    let mut limit = 0;
    let mut selectors = Vec::new();
    for (key, value) in params {
        match key.as_str() {
            "start" => start = Some(query::timestamp(&value)?),
            "end" => end = Some(query::timestamp(&value)?),
            "limit" => limit = value.parse().map_err(|_| "invalid limit")?,
            "match[]" => selectors.push(query::parse_discovery_selector(&value)?),
            _ => {}
        }
    }
    if let (Some(start), Some(end)) = (start, end)
        && start > end
    {
        return Err("start must not exceed end".into());
    }
    Ok((selectors, limit))
}

async fn discover(db: Arc<Db>, name: Option<String>, params: CatalogParams) -> Response {
    if !db.is_ready() {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable; restart required".into(),
        );
    }
    let (selectors, limit) = match discovery_params(params) {
        Ok(params) => params,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let result = tokio::task::spawn_blocking(move || {
        let mut values = if selectors.is_empty() {
            let mut values = db.catalog(name.as_deref());
            values.sort_unstable();
            values
        } else {
            let mut values = BTreeSet::new();
            for selector in selectors {
                for labels in db.series_labels(&selector.index_matchers()) {
                    if !selector.matches(&labels) {
                        continue;
                    }
                    match &name {
                        None => values.extend(labels.iter().map(|(key, _)| key.to_owned())),
                        Some(name) => values.extend(labels.get(name).map(str::to_owned)),
                    }
                }
            }
            values.into_iter().collect()
        };
        if limit > 0 {
            values.truncate(limit);
        }
        values
    })
    .await;
    match result {
        Ok(values) => Json(json!({"status":"success","data":values})).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

fn error(status: StatusCode, message: String) -> Response {
    (status, Json(json!({"status":"error","errorType":if status==StatusCode::BAD_REQUEST {"bad_data"} else {"execution"},"error":message}))).into_response()
}
async fn run(db: Arc<Db>, p: HashMap<String, String>, instant: bool, lookback_ms: u64) -> Response {
    if !db.is_ready() {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable; restart required".into(),
        );
    }
    let params = || -> Result<_, String> {
        let get = |k: &str| {
            p.get(k)
                .map(String::as_str)
                .ok_or_else(|| format!("missing {k}"))
        };
        let expr = query::parse(get("query")?)?;
        let (start, end, step) = if instant {
            let t = match p.get("time") {
                Some(t) => query::timestamp(t)?,
                None => SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64,
            };
            (t, t, 1)
        } else {
            (
                query::timestamp(get("start")?)?,
                query::timestamp(get("end")?)?,
                query::step(get("step")?)?,
            )
        };
        if start > end {
            return Err("start must not exceed end".into());
        }
        Ok((expr, start, end, step))
    };
    let (expr, start, end, step) = match params() {
        Ok(p) => p,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    match tokio::task::spawn_blocking(move || {
        query::evaluate(&db, expr, start, end, step, instant, lookback_ms)
    })
    .await
    {
        Ok(Ok(data)) => Json(json!({"status":"success","data":data})).into_response(),
        Ok(Err(e)) => error(
            if e.contains("restart required") {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::UNPROCESSABLE_ENTITY
            },
            e,
        ),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}
pub async fn buildinfo() -> Json<serde_json::Value> {
    Json(
        json!({"status":"success","data":{"version":env!("CARGO_PKG_VERSION"),"revision":"","branch":"","buildUser":"tsdb","buildDate":"","goVersion":""}}),
    )
}

pub async fn ready(State(db): State<Arc<Db>>) -> StatusCode {
    if db.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}
