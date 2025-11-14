//! Integration tests for elacsym

use elacsym_builder::{BuilderConfig, BuilderService};
use elacsym_core::{DistanceMetric, Metadata, Vector, VectorId};
use elacsym_index::IndexConfig;
use elacsym_metadata::{MetadataService, NamespaceMetadata, RocksDBStore};
use elacsym_storage::{BlobStorage, BlobStorageConfig, StorageBackend};
use std::sync::Arc;

#[tokio::test]
async fn test_upsert_and_query() {
    // Setup
    let storage_config = BlobStorageConfig {
        base_path: "test_slabs".to_string(),
        backend: StorageBackend::Memory,
    };
    let storage = Arc::new(BlobStorage::new(storage_config).await.unwrap());

    let temp_dir = tempfile::tempdir().unwrap();
    let metadata_store = Arc::new(RocksDBStore::new(temp_dir.path()).unwrap());
    let metadata = Arc::new(MetadataService::new(metadata_store));

    let builder_config = BuilderConfig {
        min_vectors: 10,
        max_vectors_per_slab: 100,
        build_interval_secs: 3600,
        index_config: IndexConfig::new(128, DistanceMetric::L2),
    };

    let builder = Arc::new(BuilderService::new(
        builder_config,
        Arc::clone(&storage),
        Arc::clone(&metadata),
    ));

    // Create namespace
    let ns_metadata = NamespaceMetadata {
        name: "test".to_string(),
        dimension: 128,
        vector_count: 0,
        partition_count: 0,
    };
    metadata.upsert_namespace(ns_metadata).await.unwrap();

    // Insert vectors
    let mut vectors = Vec::new();
    for i in 0..20 {
        let values: Vec<f32> = (0..128).map(|j| (i * 128 + j) as f32 / 1000.0).collect();
        let vector = Vector::new(
            VectorId::from(format!("vec_{}", i)),
            values,
            Metadata::new(),
            "test",
        );
        vectors.push(vector);
    }

    builder.add_vectors("test", vectors).await.unwrap();

    // Query
    let freshness_layer = builder.get_freshness_layer("test").await;
    assert_eq!(freshness_layer.len(), 20);

    let query = vec![0.5; 128];
    let search_config = elacsym_index::SearchConfig::new(5, 4);
    let results = freshness_layer.search(&query, &search_config).await.unwrap();

    assert!(!results.is_empty());
    assert!(results.len() <= 5);
}

#[tokio::test]
async fn test_namespace_operations() {
    let temp_dir = tempfile::tempdir().unwrap();
    let metadata_store = Arc::new(RocksDBStore::new(temp_dir.path()).unwrap());
    let metadata = Arc::new(MetadataService::new(metadata_store));

    // Create namespace
    let ns_metadata = NamespaceMetadata {
        name: "test_ns".to_string(),
        dimension: 256,
        vector_count: 0,
        partition_count: 0,
    };
    metadata.upsert_namespace(ns_metadata.clone()).await.unwrap();

    // Retrieve namespace
    let retrieved = metadata.get_namespace("test_ns").await.unwrap();
    assert!(retrieved.is_some());
    let retrieved_ns = retrieved.unwrap();
    assert_eq!(retrieved_ns.name, "test_ns");
    assert_eq!(retrieved_ns.dimension, 256);

    // List namespaces
    let namespaces = metadata.list_namespaces().await.unwrap();
    assert!(namespaces.contains(&"test_ns".to_string()));
}

#[test]
fn test_vector_operations() {
    let vector = Vector::new(
        VectorId::from("test_vec"),
        vec![1.0, 2.0, 3.0],
        Metadata::new(),
        "default",
    );

    assert_eq!(vector.dimension(), 3);
    assert_eq!(vector.id.to_string(), "test_vec");
    assert_eq!(vector.namespace, "default");
}
