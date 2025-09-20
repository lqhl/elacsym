use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use elax_api::ApiServer;
use elax_store::LocalStore;
use http_body_util::BodyExt;
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};
use tower::ServiceExt; // for oneshot

fn temp_store() -> LocalStore {
    let mut path = std::env::temp_dir();
    let unique = format!(
        "elax-api-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    path.push(unique);
    LocalStore::new(path).with_fsync(false)
}

#[tokio::test]
async fn write_and_query_round_trip() {
    let store = temp_store();
    let server = ApiServer::new(store);
    let app = server.router();

    let write_body = json!({
        "upserts": [
            { "id": "doc-1", "vector": [1.0, 0.0] },
            { "id": "doc-2", "vector": [0.0, 1.0] }
        ]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v2/namespaces/test")
                .header("content-type", "application/json")
                .body(Body::from(write_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    if status != StatusCode::OK {
        panic!(
            "write failed: status={} body={}",
            status,
            String::from_utf8_lossy(&bytes)
        );
    }
    let write_json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let wal_sequence = write_json["wal_sequence"].as_u64().unwrap();

    let query_body = json!({
        "vector": [1.0, 0.0],
        "top_k": 1,
        "min_wal_sequence": wal_sequence
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v2/namespaces/test/query")
                .header("content-type", "application/json")
                .body(Body::from(query_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    if status != StatusCode::OK {
        panic!(
            "query failed: status={} body={}",
            status,
            String::from_utf8_lossy(&bytes)
        );
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let hits = value["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["id"], "doc-1");
}
