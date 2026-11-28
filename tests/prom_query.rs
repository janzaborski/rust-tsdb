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
    router, router_with_query_lookback,
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
    let (status, result) = request_json(
        &app,
        "/api/v1/query_range?query=sum_over_time%28temperature_celsius%5B2s%5D%29&start=2&end=3&step=1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        result["data"],
        json!({"resultType":"matrix","result":[{"metric":{"host":"a"},"values":[[2.0,"3"],[3.0,"5"]]}]})
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

async fn request_json(
    app: &axum::Router,
    uri: &str,
    form: Option<&'static str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().uri(uri);
    let body = match form {
        Some(form) => {
            request = request
                .method("POST")
                .header("content-type", "application/x-www-form-urlencoded");
            Body::from(form)
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn selector_lookback_is_configurable_for_instant_and_range_queries() {
    let db = Arc::new(Db::new());
    db.write(WriteBatch {
        series: vec![(
            LabelSet::from_pairs([("__name__", "cpu")]),
            vec![Sample::new(1000, 4.0)],
        )],
    })
    .unwrap();

    let short = router_with_query_lookback(db.clone(), 500);
    for (uri, form, expected) in [
        (
            "/api/v1/query?query=cpu&time=1.5",
            None,
            json!([{"metric":{"__name__":"cpu"},"value":[1.5,"4"]}]),
        ),
        ("/api/v1/query", Some("query=cpu&time=1.501"), json!([])),
        (
            "/api/v1/query_range?query=cpu&start=1&end=2&step=500ms",
            None,
            json!([{"metric":{"__name__":"cpu"},"values":[[1.0,"4"],[1.5,"4"]]}]),
        ),
        (
            "/api/v1/query_range",
            Some("query=avg_over_time%28cpu%5B2s%5D%29&start=2&end=3&step=1s"),
            json!([{"metric":{},"values":[[2.0,"4"]]}]),
        ),
    ] {
        let (status, body) = request_json(&short, uri, form).await;
        assert_eq!(status, StatusCode::OK, "{uri} {body}");
        assert_eq!(body["data"]["result"], expected, "{uri}");
    }

    let exact = router_with_query_lookback(db.clone(), 0);
    let (_, body) = request_json(
        &exact,
        "/api/v1/query_range?query=cpu&start=1&end=2&step=1s",
        None,
    )
    .await;
    assert_eq!(body["data"]["result"][0]["values"], json!([[1.0, "4"]]));

    let default = router(db);
    let (_, body) = request_json(&default, "/api/v1/query?query=cpu&time=301", None).await;
    assert_eq!(body["data"]["result"][0]["value"], json!([301.0, "4"]));
}

#[tokio::test]
async fn discovers_metrics_and_labels_from_the_catalog_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open(dir.path(), 100).unwrap());
    let app = router(db.clone());
    let metrics = "/api/v1/label/__name__/values";
    let (status, body) = request_json(&app, metrics, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"status":"success","data":[]}));

    db.write(WriteBatch {
        series: vec![
            (
                LabelSet::from_pairs([("__name__", "memory"), ("host", "z")]),
                vec![Sample::new(1, 1.0)],
            ),
            (
                LabelSet::from_pairs([("__name__", "cpu"), ("host", "b"), ("region", "west")]),
                vec![Sample::new(1, 1.0)],
            ),
            (
                LabelSet::from_pairs([("__name__", "cpu"), ("host", "a"), ("region", "west")]),
                vec![Sample::new(1, 1.0)],
            ),
        ],
    })
    .unwrap();
    db.flush().unwrap();
    drop(app);
    drop(db);
    let app = router(Arc::new(Db::open(dir.path(), 100).unwrap()));
    for (uri, form, expected) in [
        (metrics, None, json!(["cpu", "memory"])),
        (
            "/api/v1/labels",
            None,
            json!(["__name__", "host", "region"]),
        ),
        (
            "/api/v1/labels",
            Some("match%5B%5D=memory"),
            json!(["__name__", "host"]),
        ),
        (
            "/api/v1/label/host/values?match%5B%5D=%7B__name__%3D%22cpu%22%7D&start=10&end=20",
            None,
            json!(["a", "b"]),
        ),
        (
            "/api/v1/label/host/values?match%5B%5D=memory&match%5B%5D=cpu&match%5B%5D=cpu",
            None,
            json!(["a", "b", "z"]),
        ),
        (
            "/api/v1/label/host/values?match%5B%5D=memory&match%5B%5D=cpu&match%5B%5D=cpu&limit=2",
            None,
            json!(["a", "b"]),
        ),
        ("/api/v1/label/region/values?limit=0", None, json!(["west"])),
        ("/api/v1/label/unknown/values", None, json!([])),
        ("/api/v1/metadata", None, json!({})),
    ] {
        let (status, body) = request_json(&app, uri, form).await;
        assert_eq!(status, StatusCode::OK, "{uri} {body}");
        assert_eq!(body, json!({"status":"success","data":expected}), "{uri}");
    }
    for uri in [
        "/api/v1/labels?match%5B%5D=1%2B1",
        "/api/v1/labels?match%5B%5D=%7Bhost%3D~%22.%2B%22%7D",
        "/api/v1/labels?limit=-1",
        "/api/v1/labels?start=2&end=1",
    ] {
        let (status, body) = request_json(&app, uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(body["errorType"], "bad_data");
    }
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
            "/api/v1/query?query=sum_over_time%28cpu%5B0s%5D%29",
            400,
            "",
        ),
        (
            "/api/v1/query?query=count_over_time%28cpu%5B5m%5D%29&time=1",
            200,
            "vector",
        ),
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
