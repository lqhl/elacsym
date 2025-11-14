//! elacsym server

use elacsym_api::{ApiServer, VectorDBService};
use elacsym_builder::{BuilderConfig as ServiceBuilderConfig, BuilderService, BuilderWorker};
use elacsym_config::Config;
use elacsym_core::DistanceMetric;
use elacsym_index::IndexConfig as ServiceIndexConfig;
use elacsym_metadata::{MetadataService, RocksDBStore};
use elacsym_storage::{BlobStorage, BlobStorageConfig, StorageBackend};
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load configuration
    let config = if let Some(config_path) = std::env::args().nth(1) {
        println!("Loading configuration from: {}", config_path);
        Config::from_file(config_path)?
    } else {
        println!("No configuration file specified, using environment or defaults");
        println!("Usage: elacsym-server [config.toml]");
        println!("Environment variable: ELACSYM_CONFIG=path/to/config.toml");
        Config::from_env()
    };

    // Initialize tracing
    let log_level = &config.server.log_level;
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level));

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .init();

    info!("Starting elacsym server");
    info!("Configuration: {:?}", config);

    // Create blob storage
    let storage_backend = match config.storage.backend.as_str() {
        "memory" => {
            info!("Using in-memory storage");
            StorageBackend::Memory
        }
        "local" => {
            let path = config.storage.local_path.clone()
                .unwrap_or_else(|| "./data/slabs".to_string());
            info!("Using local storage at: {}", path);
            std::fs::create_dir_all(&path)?;
            StorageBackend::Local { path }
        }
        "s3" => {
            let bucket = config.storage.s3_bucket.clone()
                .ok_or_else(|| anyhow::anyhow!("s3_bucket must be set for s3 backend"))?;
            let region = config.storage.s3_region.clone()
                .ok_or_else(|| anyhow::anyhow!("s3_region must be set for s3 backend"))?;
            info!("Using S3 storage: bucket={}, region={}", bucket, region);
            StorageBackend::S3 {
                bucket,
                region,
                endpoint: config.storage.s3_endpoint.clone(),
            }
        }
        _ => {
            return Err(anyhow::anyhow!("Unknown storage backend: {}", config.storage.backend));
        }
    };

    let storage_config = BlobStorageConfig {
        base_path: config.storage.base_path.clone(),
        backend: storage_backend,
    };
    let storage = Arc::new(BlobStorage::new(storage_config).await?);

    // Create metadata store
    info!("Creating metadata store at: {}", config.metadata.path);
    std::fs::create_dir_all(&config.metadata.path)?;
    let metadata_store = Arc::new(RocksDBStore::new(&config.metadata.path)?);
    let metadata = Arc::new(MetadataService::new(metadata_store));

    // Create builder service
    let builder_config = ServiceBuilderConfig {
        min_vectors: config.builder.min_vectors,
        max_vectors_per_slab: config.builder.max_vectors_per_slab,
        build_interval_secs: config.builder.build_interval_secs,
        index_config: ServiceIndexConfig::new(
            config.index.default_dimension,
            config.index.get_metric(),
        ),
    };

    let builder = Arc::new(BuilderService::new(
        builder_config,
        Arc::clone(&storage),
        Arc::clone(&metadata),
    ));

    // Start builder worker
    info!("Starting builder worker");
    let worker = BuilderWorker::new(Arc::clone(&builder));
    tokio::spawn(async move {
        worker.start().await;
    });

    // Create vector database service
    let db_service = Arc::new(VectorDBService::new(
        Arc::clone(&storage),
        Arc::clone(&metadata),
        Arc::clone(&builder),
    ));

    // Initialize service
    db_service.init().await?;

    // Create default namespace if it doesn't exist
    if db_service.create_namespace("default".to_string(), config.index.default_dimension).await.is_ok() {
        info!("Created default namespace with dimension {}", config.index.default_dimension);
    }

    // Start API server
    let addr = format!("{}:{}", config.server.host, config.server.port).parse()?;
    let server = ApiServer::new(addr, db_service);

    println!("\n🚀 elacsym server starting on http://{}:{}", config.server.host, config.server.port);
    println!("\n📋 API endpoints:");
    println!("  GET  /health              - Health check");
    println!("  POST /namespaces          - Create namespace");
    println!("  POST /vectors/upsert      - Insert/update vectors");
    println!("  POST /vectors/query       - Query for similar vectors");
    println!("  POST /vectors/fetch       - Fetch vectors by ID");
    println!("  POST /vectors/delete      - Delete vectors");
    println!("\n💾 Storage: {}", config.storage.backend);
    println!("📊 Default dimension: {}", config.index.default_dimension);
    println!("📈 Distance metric: {}", config.index.default_metric);
    println!();

    server.run().await?;

    Ok(())
}
