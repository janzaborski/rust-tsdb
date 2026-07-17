use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tempfile::tempdir;
use tower::ServiceExt;
use tsdb::storage::wal::WalConfig;
use tsdb::{Db, router};

fn test_wal_config(dir: &std::path::Path) -> WalConfig {
    WalConfig {
        dir: dir.join("wal"),
        segment_max_bytes: 1024 * 1024,
    }
}

async fn write_cpu_samples(app: axum::Router) {
    let write_body = json!([{
        "labels": {
            "__name__": "cpu_usage",
            "host": "abc"
        },
        "samples": [
            {"t": 1719000000000u64, "v": 0.5},
            {"t": 1719000001000u64, "v": 1.5}
        ]
    }]);

    let response = app
        .oneshot(
            Request::post("/api/v1/write_json")
                .header("content-type", "application/json")
                .body(Body::from(write_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

async fn read_cpu_samples(app: axum::Router) -> Value {
    let response = app
        .oneshot(
            Request::get(
                "/api/v1/read\
                 ?name=cpu_usage\
                 &host=abc\
                 &start=1719000000000\
                 &end=1719000002000",
            )
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();

    serde_json::from_slice(&bytes).unwrap()
}

fn assert_cpu_samples(body: &Value) {
    assert_eq!(body["status"], "success");
    assert_eq!(body["data"]["resultType"], "matrix");

    let result = body["data"]["result"].as_array().unwrap();
    assert_eq!(result.len(), 1);

    let series = &result[0];

    assert_eq!(series["metric"]["__name__"], "cpu_usage");
    assert_eq!(series["metric"]["host"], "abc");

    let values = series["values"].as_array().unwrap();

    assert_eq!(values.len(), 2);
    assert_eq!(values[0], json!([1719000000.0, "0.5"]));
    assert_eq!(values[1], json!([1719000001.0, "1.5"]));
}

#[tokio::test]
async fn write_then_read_roundtrips() {
    let dir = tempdir().unwrap();

    let db = Db::open(test_wal_config(dir.path())).unwrap();
    let app = router(Arc::new(db));

    write_cpu_samples(app.clone()).await;

    let body = read_cpu_samples(app).await;

    assert_cpu_samples(&body);
}

#[tokio::test]
async fn data_survives_database_restart() {
    let dir = tempdir().unwrap();

    {
        let db = Db::open(test_wal_config(dir.path())).unwrap();
        let app = router(Arc::new(db));

        write_cpu_samples(app).await;
    }

    {
        let db = Db::open(test_wal_config(dir.path())).unwrap();
        let app = router(Arc::new(db));

        let body = read_cpu_samples(app).await;

        assert_cpu_samples(&body);
    }
}
