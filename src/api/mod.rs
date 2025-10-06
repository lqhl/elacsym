//! HTTP API server

use axum::{
    routing::{get, post, put},
    Router,
};
use std::sync::Arc;

use crate::namespace::NamespaceManager;
use crate::sharding::IndexerCluster;

pub mod handlers;
pub mod state;

pub use state::AppState;

/// Build the API router for single-node mode
pub fn create_router(manager: Arc<NamespaceManager>) -> Router {
    let state = AppState::single_node(manager);
    Router::new()
        .route("/health", get(handlers::health))
        .nest(
            "/v1",
            Router::new()
                .route("/namespaces/:namespace", put(handlers::create_namespace))
                .route("/namespaces/:namespace/upsert", post(handlers::upsert))
                .route("/namespaces/:namespace/query", post(handlers::query)),
        )
        .with_state(state)
}

/// Build the API router for multi-node mode
pub fn create_router_with_cluster(
    manager: Arc<NamespaceManager>,
    cluster: Arc<IndexerCluster>,
) -> Router {
    let state = AppState::multi_node(manager, cluster);
    Router::new()
        .route("/health", get(handlers::health))
        .nest(
            "/v1",
            Router::new()
                .route("/namespaces/:namespace", put(handlers::create_namespace))
                .route("/namespaces/:namespace/upsert", post(handlers::upsert))
                .route("/namespaces/:namespace/query", post(handlers::query)),
        )
        .with_state(state)
}
