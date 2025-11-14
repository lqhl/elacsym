//! API handlers

use crate::models::*;
use crate::service::VectorDBService;
use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use elacsym_metadata::MetadataStore;
use std::sync::Arc;

/// Upsert handler
pub async fn upsert_handler<S: MetadataStore>(
    State(service): State<Arc<VectorDBService<S>>>,
    Json(req): Json<UpsertRequest>,
) -> Result<Json<crate::service::UpsertResponse>, ApiError> {
    let response = service.upsert(req).await?;
    Ok(Json(response))
}

/// Query handler
pub async fn query_handler<S: MetadataStore>(
    State(service): State<Arc<VectorDBService<S>>>,
    Json(req): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, ApiError> {
    let response = service.query(req).await?;
    Ok(Json(response))
}

/// Fetch handler
pub async fn fetch_handler<S: MetadataStore>(
    State(service): State<Arc<VectorDBService<S>>>,
    Json(req): Json<FetchRequest>,
) -> Result<Json<crate::service::FetchResponse>, ApiError> {
    let namespace = if req.namespace.is_empty() {
        "default"
    } else {
        &req.namespace
    };

    let response = service.fetch(namespace, req.ids).await?;
    Ok(Json(response))
}

/// Delete handler
pub async fn delete_handler<S: MetadataStore>(
    State(service): State<Arc<VectorDBService<S>>>,
    Json(req): Json<DeleteRequest>,
) -> Result<Json<crate::service::DeleteResponse>, ApiError> {
    let namespace = if req.namespace.is_empty() {
        "default"
    } else {
        &req.namespace
    };

    let response = service.delete(namespace, req.ids).await?;
    Ok(Json(response))
}

/// Create namespace handler
pub async fn create_namespace_handler<S: MetadataStore>(
    State(service): State<Arc<VectorDBService<S>>>,
    Json(req): Json<CreateNamespaceRequest>,
) -> Result<StatusCode, ApiError> {
    service.create_namespace(req.name, req.dimension).await?;
    Ok(StatusCode::CREATED)
}

/// API error type
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(ErrorResponse {
            error: self.status.to_string(),
            message: self.message,
        });

        (self.status, body).into_response()
    }
}

impl From<elacsym_core::Error> for ApiError {
    fn from(err: elacsym_core::Error) -> Self {
        let (status, message) = match err {
            elacsym_core::Error::VectorNotFound(id) => {
                (StatusCode::NOT_FOUND, format!("Vector not found: {}", id))
            }
            elacsym_core::Error::NamespaceNotFound(ns) => {
                (StatusCode::NOT_FOUND, format!("Namespace not found: {}", ns))
            }
            elacsym_core::Error::DimensionMismatch { expected, actual } => (
                StatusCode::BAD_REQUEST,
                format!("Dimension mismatch: expected {}, got {}", expected, actual),
            ),
            elacsym_core::Error::InvalidNamespace(ns) => {
                (StatusCode::BAD_REQUEST, format!("Invalid namespace: {}", ns))
            }
            _ => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
        };

        ApiError { status, message }
    }
}
