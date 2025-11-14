//! API server

use crate::handlers::*;
use axum::{routing::post, Router};
use std::net::SocketAddr;
use tracing::info;

pub struct ApiServer {
    addr: SocketAddr,
}

impl ApiServer {
    pub fn new(addr: SocketAddr) -> Self {
        Self { addr }
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let app = Router::new()
            .route("/vectors/upsert", post(upsert_handler))
            .route("/vectors/query", post(query_handler));

        info!("API server listening on {}", self.addr);

        let listener = tokio::net::TcpListener::bind(self.addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }
}
