use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
use tsdb::{
    Db, WriteBatch,
    model::{LabelSet, Sample},
    router,
};

#[tokio::test]
async fn queries_merge_disk_and_memory_and_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open(dir.path(), 2).unwrap());
    let labels = LabelSet::from_pairs([("__name__", "temperature_celsius"), ("host", "a")]);
    for (t, v) in [(1000, 1.0), (2000, 2.0), (3000, 3.0)] {
        db.write(WriteBatch {
            series: vec![(labels.clone(), vec![Sample::new(t, v)])],
        })
        .unwrap();
    }
    let app = router(db.clone());
    let response = app
        .clone()
        .oneshot(
            Request::post("/api/v1/query_range")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "query=temperature_celsius%7Bhost%3D%22a%22%7D&start=1&end=3&step=1s",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(
        result["data"]["result"][0]["values"],
        json!([[1.0, "1"], [2.0, "2"], [3.0, "3"]])
    );
    db.flush().unwrap();
    drop(app);
    drop(db);
    let app = router(Arc::new(Db::open(dir.path(), 2).unwrap()));
    let response = app
        .oneshot(
            Request::get("/api/v1/query?query=temperature_celsius&time=3")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let result: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(result["data"]["result"][0]["value"], json!([3.0, "3"]));
}

#[tokio::test]
async fn health_scalar_empty_and_errors() {
    let app = router(Arc::new(Db::new()));
    for (uri, status, kind) in [
        ("/api/v1/query?query=1%2B1&time=1", 200, "scalar"),
        (
            "/api/v1/query_range?query=1%2B1&start=1&end=4&step=15",
            200,
            "matrix",
        ),
        ("/api/v1/query?query=absent&time=1", 200, "vector"),
        ("/api/v1/query?query=avg(cpu)", 400, ""),
        (
            "/api/v1/query_range?query=cpu&start=3&end=1&step=1",
            400,
            "",
        ),
        (
            "/api/v1/query_range?query=cpu&start=1&end=3&step=0",
            400,
            "",
        ),
        (
            "/api/v1/query_range?query=cpu&start=0&end=12000&step=1",
            422,
            "",
        ),
    ] {
        let response = app
            .clone()
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status, "{uri}");
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        if status == 200 {
            assert_eq!(body["data"]["resultType"], kind);
        } else {
            assert_eq!(body["status"], "error");
            assert!(body["error"].is_string());
        }
    }
    for path in ["/readyz", "/healthz", "/api/v1/status/buildinfo"] {
        assert_eq!(
            app.clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }
}
