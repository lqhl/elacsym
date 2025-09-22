use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::{engine::general_purpose, Engine as _};
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
        "rank_by": ["vector", "ANN", [1.0, 0.0]],
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

#[tokio::test]
async fn query_accepts_base64_ann_payload() {
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

    assert_eq!(response.status(), StatusCode::OK);

    let mut bytes = Vec::new();
    for value in [1.0f32, 0.0f32] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    let encoded = general_purpose::STANDARD.encode(bytes);
    let query_body = json!({
        "rank_by": ["vector", "ANN", format!("base64:{encoded}")],
        "top_k": 1
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

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let hits = value["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["id"], "doc-1");
}

#[tokio::test]
async fn write_rejects_empty_document_id() {
    let store = temp_store();
    let server = ApiServer::new(store);
    let app = server.router();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v2/namespaces/test")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "upserts": [{ "id": "", "vector": [1.0] }] }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let message = value["error"].as_str().unwrap();
    assert!(message.contains("id must not be empty"));
}

#[tokio::test]
async fn query_errors_when_consistency_unmet() {
    let store = temp_store();
    let server = ApiServer::new(store);
    let app = server.router();

    let write_body = json!({
        "upserts": [
            { "id": "doc-1", "vector": [1.0, 0.0] }
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

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let wal_sequence = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["wal_sequence"]
        .as_u64()
        .unwrap();

    let query_body = json!({
        "rank_by": ["vector", "ANN", [1.0, 0.0]],
        "top_k": 1,
        "min_wal_sequence": wal_sequence + 5
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

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let error: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(error["error"]
        .as_str()
        .unwrap()
        .contains("consistency level unmet"));
}

#[tokio::test]
async fn write_supports_columnar_and_patch_payloads() {
    let store = temp_store();
    let server = ApiServer::new(store);
    let app = server.router();

    let write_body = json!({
        "upsert_columns": [
            {
                "ids": ["doc-1", "doc-2"],
                "vector": [[1.0, 0.0], [0.0, 1.0]],
                "attributes": {
                    "category": ["news", "sports"],
                    "score": [1, 2]
                }
            }
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
    assert_eq!(response.status(), StatusCode::OK);

    let query_body = json!({
        "rank_by": ["vector", "ANN", [1.0, 0.0]],
        "top_k": 1
    });
    let response = app
        .clone()
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
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let attrs = &value["hits"][0]["attributes"];
    assert_eq!(attrs["category"], "news");
    assert_eq!(attrs["score"], 1);

    let patch_body = json!({
        "patch_columns": [
            {
                "ids": ["doc-1", "doc-2"],
                "vector": [[0.0, 1.0], [1.0, 1.0]],
                "attributes": {
                    "category": ["updated", "sports"],
                    "score": [null, 3]
                },
                "clear_attributes": ["doc-1"]
            }
        ]
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v2/namespaces/test")
                .header("content-type", "application/json")
                .body(Body::from(patch_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let query_body = json!({
        "rank_by": ["vector", "ANN", [0.0, 1.0]],
        "top_k": 1
    });
    let response = app
        .clone()
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
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["hits"][0]["id"], "doc-1");
    let attrs = &value["hits"][0]["attributes"];
    assert_eq!(attrs["category"], "updated");
    assert!(attrs.get("score").is_none());

    let query_body = json!({
        "rank_by": ["vector", "ANN", [1.0, 1.0]],
        "top_k": 1
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
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["hits"][0]["id"], "doc-2");
    let attrs = &value["hits"][0]["attributes"];
    assert_eq!(attrs["category"], "sports");
    assert_eq!(attrs["score"], 3);
}

#[tokio::test]
async fn write_allows_delete_by_filter() {
    let store = temp_store();
    let server = ApiServer::new(store);
    let app = server.router();

    let write_body = json!({
        "upserts": [
            { "id": "doc-1", "vector": [1.0, 0.0], "attributes": {"category": "news"} },
            { "id": "doc-2", "vector": [0.0, 1.0], "attributes": {"category": "sports"} }
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
    assert_eq!(response.status(), StatusCode::OK);

    let delete_body = json!({
        "delete_by_filter": [
            { "op": "eq", "field": "category", "value": "news" }
        ]
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v2/namespaces/test")
                .header("content-type", "application/json")
                .body(Body::from(delete_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let query_body = json!({
        "rank_by": ["vector", "ANN", [1.0, 0.0]],
        "top_k": 1
    });
    let response = app
        .clone()
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
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(value["hits"].as_array().unwrap().is_empty());

    let query_body = json!({
        "rank_by": ["vector", "ANN", [0.0, 1.0]],
        "top_k": 1
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
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["hits"].as_array().unwrap().len(), 1);
    assert_eq!(value["hits"][0]["id"], "doc-2");
}

#[tokio::test]
async fn write_condition_failure_returns_precondition() {
    let store = temp_store();
    let server = ApiServer::new(store);
    let app = server.router();

    let write_body = json!({
        "upserts": [
            { "id": "doc-1", "vector": [1.0, 0.0] }
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
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let wal_sequence = value["wal_sequence"].as_u64().unwrap();

    let conditional_write = json!({
        "upserts": [
            { "id": "doc-2", "vector": [0.0, 1.0] }
        ],
        "upsert_condition": { "min_wal_sequence": wal_sequence + 1 }
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v2/namespaces/test")
                .header("content-type", "application/json")
                .body(Body::from(conditional_write.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(value["error"].as_str().unwrap().contains("wal sequence"));

    let query_body = json!({
        "rank_by": ["vector", "ANN", [0.0, 1.0]],
        "top_k": 1
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
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(value["hits"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn query_eventual_allows_stale_reads() {
    let store = temp_store();
    let server = ApiServer::new(store);
    let app = server.router();

    let write_body = json!({
        "upserts": [
            { "id": "doc-1", "vector": [1.0, 0.0] }
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
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let wal_sequence = value["wal_sequence"].as_u64().unwrap();

    let query_body = json!({
        "rank_by": ["vector", "ANN", [1.0, 0.0]],
        "top_k": 1,
        "min_wal_sequence": wal_sequence + 1,
        "consistency": { "level": "eventual" }
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
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["hits"].as_array().unwrap().len(), 1);
    assert_eq!(value["hits"][0]["id"], "doc-1");
}

#[tokio::test]
async fn recall_endpoint_reports_full_recall_with_fp32_rerank() {
    let store = temp_store();
    let server = ApiServer::new(store);
    let app = server.router();

    let docs: Vec<_> = (0..12)
        .map(|i| {
            json!({
                "id": format!("doc-{i}"),
                "vector": [i as f32, (i % 3) as f32],
            })
        })
        .collect();
    let write_body = json!({ "upserts": docs });

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
    assert_eq!(response.status(), StatusCode::OK);

    let recall_body = json!({
        "num": 5,
        "top_k": 3,
        "ann_params": { "rerank_mode": "fp32", "target_recall": 1.0 }
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/namespaces/test/_debug/recall")
                .header("content-type", "application/json")
                .body(Body::from(recall_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["evaluated"].as_u64().unwrap(), 5);
    let avg_recall = value["avg_recall"].as_f64().unwrap();
    assert!((avg_recall - 1.0).abs() < 1e-6, "recall should be 1.0");
}

#[tokio::test]
async fn bm25_query_returns_hit() {
    let store = temp_store();
    let server = ApiServer::new(store);
    let app = server.router();

    let write_body = json!({
        "upserts": [
            { "id": "doc-1", "attributes": { "content": "Rust search engine overview" } },
            { "id": "doc-2", "attributes": { "content": "Hybrid search primer" } }
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
    assert_eq!(response.status(), StatusCode::OK);

    let query_body = json!({
        "rank_by": ["content", "BM25", "rust search"],
        "top_k": 1
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
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let hits = value["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["id"], "doc-1");
}

#[tokio::test]
async fn hybrid_query_merges_candidates() {
    let store = temp_store();
    let server = ApiServer::new(store);
    let app = server.router();

    let write_body = json!({
        "upserts": [
            {
                "id": "doc-vec",
                "vector": [1.0, 0.0],
                "attributes": { "content": "Rust vector search" }
            },
            {
                "id": "doc-text",
                "attributes": { "content": "Hybrid search introduction" }
            }
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
    assert_eq!(response.status(), StatusCode::OK);

    let query_body = json!({
        "rank_by": ["vector", "ANN", [1.0, 0.0]],
        "top_k": 2,
        "queries": [
            { "rank_by": ["content", "BM25", "hybrid"], "top_k": 1 }
        ]
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
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let hits = value["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2);
    let ids: Vec<_> = hits.iter().map(|hit| hit["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"doc-vec"));
    assert!(ids.contains(&"doc-text"));
}

#[tokio::test]
async fn metrics_endpoint_returns_prometheus_payload() {
    let store = temp_store();
    let server = ApiServer::new(store);
    let app = server.router();

    let write_body = json!({
        "upserts": [
            { "id": "doc-1", "vector": [0.0, 1.0] }
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
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("elax_metrics_up"));
}
