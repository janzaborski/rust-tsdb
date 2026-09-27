use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;
use tsdb::{
    Db, WriteBatch,
    model::{Label, LabelSet, Sample},
    router,
};

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

#[tokio::test]
async fn read_rejects_malformed_and_reversed_time_ranges() {
    let app = router(Arc::new(Db::new()));
    for query in [
        "start=oops",
        "end=bad",
        "start=",
        "end=",
        "start=-1",
        "end=-1",
        "start=1.5",
        "end=1.5",
        "start=18446744073709551616",
        "end=18446744073709551616",
        "start=200&end=100",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/read?{query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(
            !bytes.is_empty(),
            "{query} should explain the invalid input"
        );
    }
}

#[tokio::test]
async fn read_preserves_default_and_inclusive_time_bounds() {
    let db = Arc::new(Db::new());
    db.write(WriteBatch {
        series: vec![(
            LabelSet::from_labels([Label::new("__name__", "cpu")]),
            vec![
                Sample::new(0, 0.0),
                Sample::new(100, 1.0),
                Sample::new(u64::MAX, 2.0),
            ],
        )],
    })
    .unwrap();
    let app = router(db);

    for (uri, expected_count) in [
        ("/api/v1/read", 3),
        ("/api/v1/read?start=100", 2),
        ("/api/v1/read?end=100", 2),
        ("/api/v1/read?start=100&end=100", 1),
        ("/api/v1/read?start=0&end=0", 1),
        (
            "/api/v1/read?start=18446744073709551615&end=18446744073709551615",
            1,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            body["data"]["result"][0]["values"]
                .as_array()
                .unwrap()
                .len(),
            expected_count,
            "{uri}"
        );
    }
}

#[tokio::test]
async fn writes_discard_old_and_duplicate_samples_and_still_return_success() {
    let app = router(Arc::new(Db::new()));
    for samples in [
        json!([{"t": 200, "v": 2}, {"t": 100, "v": 1}, {"t": 200, "v": 9}, {"t": 300, "v": 3}]),
        json!([{"t": 100, "v": 9}, {"t": 300, "v": 9}]),
        json!([{"t": 250, "v": 9}, {"t": 400, "v": 4}]),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/v1/write_json")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!([{"labels": {"__name__": "cpu"}, "samples": samples}]).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
    let response = app
        .oneshot(
            Request::get("/api/v1/read?name=cpu")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["data"]["result"].as_array().unwrap().len(), 1);
    assert_eq!(
        body["data"]["result"][0]["values"],
        json!([[0.2, "2"], [0.3, "3"], [0.4, "4"]])
    );
}
