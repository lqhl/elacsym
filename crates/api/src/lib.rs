#![allow(dead_code)]

//! HTTP layer powered by Axum.

use axum::{routing::get, Router};
use common::{Error, Result};
use tracing::instrument;

/// Builds the API router with stubbed handlers for now.
pub fn router() -> Router {
    Router::new().route("/health", get(health))
}

#[instrument]
async fn health() -> &'static str {
    "ok"
}

/// Placeholder entrypoint for wiring up background services.
#[instrument]
pub async fn serve() -> Result<()> {
    Err(Error::Message(
        "HTTP server not yet implemented".to_string(),
    ))
}
