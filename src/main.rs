//! Elacsym server binary

use std::env;
use std::sync::Arc;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use elacsym::namespace::NamespaceManager;
use elacsym::storage::local::LocalStorage;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "elacsym=debug,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Get storage path from environment or use default
    let storage_path = env::var("ELACSYM_STORAGE_PATH")
        .unwrap_or_else(|_| "./data".to_string());

    tracing::info!("Using storage path: {}", storage_path);

    // Create storage backend (local for now)
    let storage = Arc::new(LocalStorage::new(&storage_path)?);

    // Create namespace manager
    let manager = Arc::new(NamespaceManager::new(storage));

    // Create API router
    let app = elacsym::api::create_router(manager);

    // Start server
    let addr = "0.0.0.0:3000";
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("Listening on {}", addr);

    axum::serve(listener, app).await?;

    Ok(())
}
