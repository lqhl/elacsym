//! Elacsym server binary

use std::env;
use std::sync::Arc;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use elacsym::cache::{CacheConfig, CacheManager};
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
    let storage_path = env::var("ELACSYM_STORAGE_PATH").unwrap_or_else(|_| "./data".to_string());

    tracing::info!("Using storage path: {}", storage_path);

    // Create storage backend (local for now)
    let storage = Arc::new(LocalStorage::new(&storage_path)?);

    // Create cache (optional - can be disabled with env var)
    let cache = if env::var("ELACSYM_DISABLE_CACHE").is_ok() {
        tracing::info!("Cache disabled by environment variable");
        None
    } else {
        let cache_config = CacheConfig {
            memory_size: 4 * 1024 * 1024 * 1024, // 4GB
            disk_size: 100 * 1024 * 1024 * 1024, // 100GB
            disk_path: env::var("ELACSYM_CACHE_PATH")
                .unwrap_or_else(|_| "/tmp/elacsym-cache".to_string()),
        };

        tracing::info!(
            "Initializing cache: memory={}GB, disk={}GB, path={}",
            cache_config.memory_size / (1024 * 1024 * 1024),
            cache_config.disk_size / (1024 * 1024 * 1024),
            cache_config.disk_path
        );

        match CacheManager::new(cache_config).await {
            Ok(cache) => {
                tracing::info!("Cache initialized successfully");
                Some(Arc::new(cache))
            }
            Err(e) => {
                tracing::warn!("Failed to initialize cache: {}. Running without cache.", e);
                None
            }
        }
    };

    // Create namespace manager
    let manager = if let Some(cache) = cache {
        Arc::new(NamespaceManager::with_cache(storage, cache))
    } else {
        Arc::new(NamespaceManager::new(storage))
    };

    // Create API router
    let app = elacsym::api::create_router(manager);

    // Start server
    let addr = "0.0.0.0:3000";
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("Listening on {}", addr);

    axum::serve(listener, app).await?;

    Ok(())
}
