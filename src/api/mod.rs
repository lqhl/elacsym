//! HTTP API server

use axum::{
    routing::{get, post, put},
    Router,
};
use std::sync::Arc;

use crate::namespace::NamespaceManager;

pub mod handlers;

/// Build the API router
pub fn create_router(manager: Arc<NamespaceManager>) -> Router {
    Router::new()
        .route("/health", get(handlers::health))
        .nest(
            "/v1",
            Router::new()
                .route("/namespaces/:namespace", put(handlers::create_namespace))
                .route("/namespaces/:namespace/upsert", post(handlers::upsert))
                .route("/namespaces/:namespace/query", post(handlers::query)),
        )
        .with_state(manager)
}
