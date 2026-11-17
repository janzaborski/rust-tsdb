use crate::{Db, query};
use axum::{
    Json,
    extract::{
        Form, Query, State,
        rejection::{FormRejection, QueryRejection},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

pub async fn instant_get(
    State(db): State<Arc<Db>>,
    params: Result<Query<HashMap<String, String>>, QueryRejection>,
) -> Response {
    match params {
        Ok(Query(p)) => run(db, p, true).await,
        Err(e) => error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}
pub async fn instant_post(
    State(db): State<Arc<Db>>,
    params: Result<Form<HashMap<String, String>>, FormRejection>,
) -> Response {
    match params {
        Ok(Form(p)) => run(db, p, true).await,
        Err(e) => error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}
pub async fn range_get(
    State(db): State<Arc<Db>>,
    params: Result<Query<HashMap<String, String>>, QueryRejection>,
) -> Response {
    match params {
        Ok(Query(p)) => run(db, p, false).await,
        Err(e) => error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}
pub async fn range_post(
    State(db): State<Arc<Db>>,
    params: Result<Form<HashMap<String, String>>, FormRejection>,
) -> Response {
    match params {
        Ok(Form(p)) => run(db, p, false).await,
        Err(e) => error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}
fn error(status: StatusCode, message: String) -> Response {
    (status, Json(json!({"status":"error","errorType":if status==StatusCode::BAD_REQUEST {"bad_data"} else {"execution"},"error":message}))).into_response()
}
async fn run(db: Arc<Db>, p: HashMap<String, String>, instant: bool) -> Response {
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
    match tokio::task::spawn_blocking(move || query::evaluate(&db, expr, start, end, step, instant))
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
