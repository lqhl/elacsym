//! Simple elacsym server example

use elacsym_api::{ApiServer, VectorDBService};
use elacsym_builder::{BuilderConfig, BuilderService, BuilderWorker};
use elacsym_core::DistanceMetric;
use elacsym_index::IndexConfig;
use elacsym_metadata::{MetadataService, RocksDBStore};
use elacsym_storage::{BlobStorage, BlobStorageConfig, StorageBackend};
use std::sync::Arc;
use tracing_subscriber;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    // Create blob storage (in-memory for example)
    let storage_config = BlobStorageConfig {
        base_path: "slabs".to_string(),
        backend: StorageBackend::Memory,
    };
    let storage = Arc::new(BlobStorage::new(storage_config).await?);

    // Create metadata store
    let metadata_store = Arc::new(RocksDBStore::new("/tmp/elacsym_metadata")?);
    let metadata = Arc::new(MetadataService::new(metadata_store));

    // Create builder service
    let builder_config = BuilderConfig {
        min_vectors: 100,
        max_vectors_per_slab: 10_000,
        build_interval_secs: 60,
        index_config: IndexConfig::new(128, DistanceMetric::L2),
    };

    let builder = Arc::new(BuilderService::new(
        builder_config,
        Arc::clone(&storage),
        Arc::clone(&metadata),
    ));

    // Start builder worker
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

    // Create default namespace
    db_service
        .create_namespace("default".to_string(), 128)
        .await?;

    // Start API server
    let addr = "0.0.0.0:8080".parse()?;
    let server = ApiServer::new(addr, db_service);

    println!("Starting elacsym server on http://0.0.0.0:8080");
    println!("API endpoints:");
    println!("  GET  /health");
    println!("  POST /namespaces");
    println!("  POST /vectors/upsert");
    println!("  POST /vectors/query");
    println!("  POST /vectors/fetch");
    println!("  POST /vectors/delete");

    server.run().await?;

    Ok(())
}
