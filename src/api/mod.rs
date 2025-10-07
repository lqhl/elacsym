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
pub use state::NodeRole;

/// Build the API router using the provided application state
pub fn create_router(state: AppState) -> Router {
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

/// Convenience helper for single-node deployments
pub fn create_single_node_router(manager: Arc<NamespaceManager>) -> Router {
    create_router(AppState::single_node(manager))
}

/// Convenience helper for clustered deployments
pub fn create_cluster_router(
    manager: Arc<NamespaceManager>,
    cluster: Arc<IndexerCluster>,
    role: NodeRole,
) -> Router {
    create_router(AppState::multi_node(manager, cluster, role))
}
