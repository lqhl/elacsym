//! API handlers

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;

use crate::namespace::NamespaceManager;
use crate::query::{QueryRequest, QueryResponse, QueryResult};
use crate::types::{Document, Schema};

/// Health check with system status
pub async fn health(
    State(manager): State<Arc<NamespaceManager>>,
) -> Result<Json<HealthResponse>, (StatusCode, String)> {
    let namespaces = manager.list_namespaces().await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(HealthResponse {
        status: "healthy".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        namespaces: namespaces.len(),
    }))
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub namespaces: usize,
}

/// Create or update namespace
pub async fn create_namespace(
    State(manager): State<Arc<NamespaceManager>>,
    Path(namespace): Path<String>,
    Json(payload): Json<CreateNamespaceRequest>,
) -> Result<Json<CreateNamespaceResponse>, (StatusCode, String)> {
    manager
        .create_namespace(namespace.clone(), payload.schema)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(CreateNamespaceResponse {
        namespace,
        created: true,
    }))
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
    State(manager): State<Arc<NamespaceManager>>,
    Path(namespace): Path<String>,
    Json(payload): Json<UpsertRequest>,
) -> Result<Json<UpsertResponse>, (StatusCode, String)> {
    let ns = manager
        .get_namespace(&namespace)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;

    let count = ns
        .upsert(payload.documents)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(UpsertResponse { count }))
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
pub async fn query(
    State(manager): State<Arc<NamespaceManager>>,
    Path(namespace): Path<String>,
    Json(payload): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, (StatusCode, String)> {
    let start = Instant::now();

    let ns = manager
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
