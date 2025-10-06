//! API handlers

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use std::time::Instant;

use crate::api::AppState;
use crate::query::{QueryRequest, QueryResponse, QueryResult};
use crate::types::{Document, Schema};

/// Health check with system status
pub async fn health(
    State(state): State<AppState>,
) -> Result<Json<HealthResponse>, (StatusCode, String)> {
    let namespaces = state
        .manager
        .list_namespaces()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(HealthResponse {
        status: "healthy".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        node_id: state.node_id().to_string(),
        namespaces: namespaces.len(),
    }))
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub node_id: String,
    pub namespaces: usize,
}

/// Create or update namespace
pub async fn create_namespace(
    State(state): State<AppState>,
    Path(namespace): Path<String>,
    Json(payload): Json<CreateNamespaceRequest>,
) -> Result<Response, (StatusCode, String)> {
    // Check if this node should handle this namespace (for indexer nodes)
    if !state.should_handle(&namespace) {
        let responsible_node = state
            .get_responsible_node_id(&namespace)
            .unwrap_or_else(|| "unknown".to_string());

        tracing::warn!(
            "Namespace '{}' should be handled by node '{}', not this node ('{}')",
            namespace,
            responsible_node,
            state.node_id()
        );

        // Return 307 Temporary Redirect
        return Ok((
            StatusCode::TEMPORARY_REDIRECT,
            [
                (header::LOCATION, format!("/v1/namespaces/{}", namespace)),
                ("X-Correct-Indexer".parse().unwrap(), responsible_node),
            ],
        )
            .into_response());
    }

    state
        .manager
        .create_namespace(namespace.clone(), payload.schema)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(CreateNamespaceResponse {
        namespace,
        created: true,
    })
    .into_response())
}

#[derive(Debug, Deserialize)]
pub struct CreateNamespaceRequest {
    pub schema: Schema,
}

#[derive(Debug, Serialize)]
pub struct CreateNamespaceResponse {
    pub namespace: String,
    pub created: bool,
}

/// Upsert documents
pub async fn upsert(
    State(state): State<AppState>,
    Path(namespace): Path<String>,
    Json(payload): Json<UpsertRequest>,
) -> Result<Response, (StatusCode, String)> {
    // Check if this node should handle this namespace (for indexer nodes)
    if !state.should_handle(&namespace) {
        let responsible_node = state
            .get_responsible_node_id(&namespace)
            .unwrap_or_else(|| "unknown".to_string());

        tracing::warn!(
            "Namespace '{}' should be handled by node '{}', redirecting",
            namespace,
            responsible_node
        );

        // Return 307 Temporary Redirect
        return Ok((
            StatusCode::TEMPORARY_REDIRECT,
            [
                (
                    header::LOCATION,
                    format!("/v1/namespaces/{}/upsert", namespace),
                ),
                ("X-Correct-Indexer".parse().unwrap(), responsible_node),
            ],
        )
            .into_response());
    }

    let ns = state
        .manager
        .get_namespace(&namespace)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;

    let count = ns
        .upsert(payload.documents)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(UpsertResponse { count }).into_response())
}

#[derive(Debug, Deserialize)]
pub struct UpsertRequest {
    pub documents: Vec<Document>,
}

#[derive(Debug, Serialize)]
pub struct UpsertResponse {
    pub count: usize,
}

/// Query documents
///
/// Query nodes can handle any namespace (no redirect needed)
pub async fn query(
    State(state): State<AppState>,
    Path(namespace): Path<String>,
    Json(payload): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, (StatusCode, String)> {
    let start = Instant::now();

    let ns = state
        .manager
        .get_namespace(&namespace)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;

    // At least one of vector or full_text must be provided
    if payload.vector.is_none() && payload.full_text.is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            "At least one of 'vector' or 'full_text' must be provided".to_string(),
        ));
    }

    let search_results = ns
        .query(
            payload.vector.as_deref(),
            payload.full_text.as_ref(),
            payload.top_k,
            payload.filter.as_ref(),
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Convert to QueryResult format
    let results: Vec<QueryResult> = search_results
        .into_iter()
        .map(|(document, distance)| {
            let vector = if payload.include_vector {
                document.vector
            } else {
                None
            };

            // Filter attributes based on include_attributes
            let attributes = if payload.include_attributes.is_empty() {
                document.attributes
            } else {
                document
                    .attributes
                    .into_iter()
                    .filter(|(k, _)| payload.include_attributes.contains(k))
                    .collect()
            };

            QueryResult {
                id: document.id,
                score: distance,
                vector,
                attributes,
            }
        })
        .collect();

    let took_ms = start.elapsed().as_millis() as u64;

    Ok(Json(QueryResponse { results, took_ms }))
}
