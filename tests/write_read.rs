use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;
use tsdb::{Db, router};

#[tokio::test]
async fn write_then_read_roundtrips() {
    let app = router(Arc::new(Db::new()));

    let write_body = json!([{
        "labels": {"__name__": "cpu_usage", "host": "abc"},
        "samples": [
            {"t": 1719000000000u64, "v": 0.5},
            {"t": 1719000001000u64, "v": 1.5}
        ]
    }]);

    let write_res = app
        .clone()
        .oneshot(
            Request::post("/api/v1/write_json")
                .header("content-type", "application/json")
                .body(Body::from(write_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(write_res.status(), StatusCode::NO_CONTENT);

    let read_res = app
        .oneshot(
            Request::get(
                "/api/v1/read?name=cpu_usage&host=abc&start=1719000000000&end=1719000002000",
            )
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(read_res.status(), StatusCode::OK);

    let bytes = to_bytes(read_res.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["status"], "success");
    assert_eq!(body["data"]["resultType"], "matrix");

    let series = &body["data"]["result"][0];
    assert_eq!(series["metric"]["__name__"], "cpu_usage");
    assert_eq!(series["metric"]["host"], "abc");

    let values = series["values"].as_array().unwrap();
    assert_eq!(values.len(), 2);
    assert_eq!(values[0], json!([1719000000.0, "0.5"]));
    assert_eq!(values[1], json!([1719000001.0, "1.5"]));
}

#[tokio::test]
async fn empty_series_writes_do_not_break_reads_or_change_existing_samples() {
    let app = router(Arc::new(Db::new()));

    for body in [
        json!([
            {"labels": {"__name__": "empty"}, "samples": []},
            {"labels": {"__name__": "cpu"}, "samples": [{"t": 100, "v": 1.0}]}
        ]),
        json!([{ "labels": {"__name__": "cpu"}, "samples": [] }]),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/v1/write_json")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    let response = app
        .oneshot(Request::get("/api/v1/read").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        body,
        json!({
            "status": "success",
            "data": {
                "resultType": "matrix",
                "result": [{"metric": {"__name__": "cpu"}, "values": [[0.1, "1"]]}]
            }
        })
    );
}
