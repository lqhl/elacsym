//! API server

use crate::handlers::*;
use crate::service::VectorDBService;
use axum::{routing::{get, post}, Router};
use elacsym_metadata::MetadataStore;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

pub struct ApiServer<S: MetadataStore> {
    addr: SocketAddr,
    service: Arc<VectorDBService<S>>,
}

impl<S: MetadataStore + 'static> ApiServer<S> {
    pub fn new(addr: SocketAddr, service: Arc<VectorDBService<S>>) -> Self {
        Self { addr, service }
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let app = Router::new()
            .route("/health", get(health_handler))
            .route("/namespaces", post(create_namespace_handler::<S>))
            .route("/vectors/upsert", post(upsert_handler::<S>))
            .route("/vectors/query", post(query_handler::<S>))
            .route("/vectors/fetch", post(fetch_handler::<S>))
            .route("/vectors/delete", post(delete_handler::<S>))
            .with_state(self.service);

        info!("API server listening on {}", self.addr);

        let listener = tokio::net::TcpListener::bind(self.addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }
}

async fn health_handler() -> &'static str {
    "OK"
}
