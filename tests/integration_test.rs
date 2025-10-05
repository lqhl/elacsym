//! Integration tests for Elacsym
//!
//! These tests verify end-to-end functionality across multiple components.

use elacsym::cache::{CacheConfig, CacheManager};
use elacsym::namespace::{CompactionConfig, NamespaceManager};
use elacsym::query::{FilterCondition, FilterExpression, FilterOp, FullTextQuery};
use elacsym::storage::local::LocalStorage;
use elacsym::types::{
    AttributeSchema, AttributeType, AttributeValue, DistanceMetric, Document, FullTextConfig,
    Schema,
};
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;

/// Test complete workflow: create namespace → upsert → query → verify
#[tokio::test]
async fn test_end_to_end_workflow() {
    let temp_dir = TempDir::new().unwrap();
    let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());

    // Create namespace manager
    let manager = Arc::new(NamespaceManager::new(storage));

    // Define schema
    let mut attributes = HashMap::new();
    attributes.insert(
        "title".to_string(),
        AttributeSchema {
            attr_type: AttributeType::String,
            indexed: false,
            full_text: FullTextConfig::Simple(true),
        },
    );
    attributes.insert(
        "category".to_string(),
        AttributeSchema {
            attr_type: AttributeType::String,
            indexed: true,
            full_text: FullTextConfig::Simple(false),
        },
    );
    attributes.insert(
        "price".to_string(),
        AttributeSchema {
            attr_type: AttributeType::Float,
            indexed: false,
            full_text: FullTextConfig::Simple(false),
        },
    );

    let schema = Schema {
        vector_dim: 128,
        vector_metric: DistanceMetric::L2,
        attributes,
    };

    // Create namespace
    let namespace = manager
        .create_namespace("products".to_string(), schema)
        .await
        .unwrap();

    // Insert documents
    let documents = vec![
        Document {
            id: 1,
            vector: Some(vec![1.0; 128]),
            attributes: {
                let mut attrs = HashMap::new();
                attrs.insert(
                    "title".to_string(),
                    AttributeValue::String("Laptop Computer".to_string()),
                );
                attrs.insert(
                    "category".to_string(),
                    AttributeValue::String("electronics".to_string()),
                );
                attrs.insert("price".to_string(), AttributeValue::Float(999.99));
                attrs
            },
        },
        Document {
            id: 2,
            vector: Some(vec![2.0; 128]),
            attributes: {
                let mut attrs = HashMap::new();
                attrs.insert(
                    "title".to_string(),
                    AttributeValue::String("Gaming Mouse".to_string()),
                );
                attrs.insert(
                    "category".to_string(),
                    AttributeValue::String("electronics".to_string()),
                );
                attrs.insert("price".to_string(), AttributeValue::Float(49.99));
                attrs
            },
        },
        Document {
            id: 3,
            vector: Some(vec![5.0; 128]),
            attributes: {
                let mut attrs = HashMap::new();
                attrs.insert(
                    "title".to_string(),
                    AttributeValue::String("Office Chair".to_string()),
                );
                attrs.insert(
                    "category".to_string(),
                    AttributeValue::String("furniture".to_string()),
                );
                attrs.insert("price".to_string(), AttributeValue::Float(299.99));
                attrs
            },
        },
    ];

    let count = namespace.upsert(documents).await.unwrap();
    assert_eq!(count, 3);

    // Test 1: Vector search
    let query_vector = vec![1.5; 128];
    let results = namespace
        .query(Some(&query_vector), None, 2, None)
        .await
        .unwrap();
    assert_eq!(results.len(), 2);

    // Test 2: Full-text search
    let ft_query = FullTextQuery::Single {
        field: "title".to_string(),
        query: "computer".to_string(),
        weight: 1.0,
    };
    let results = namespace.query(None, Some(&ft_query), 10, None).await.unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].0.id, 1); // Laptop Computer should match

    // Test 3: Hybrid search (vector + full-text)
    let results = namespace
        .query(Some(&query_vector), Some(&ft_query), 10, None)
        .await
        .unwrap();
    assert!(!results.is_empty());

    // Test 4: Filtered search
    let filter = FilterExpression::And {
        conditions: vec![
            FilterCondition {
                field: "category".to_string(),
                op: FilterOp::Eq,
                value: AttributeValue::String("electronics".to_string()),
            },
            FilterCondition {
                field: "price".to_string(),
                op: FilterOp::Lt,
                value: AttributeValue::Float(100.0),
            },
        ],
    };

    let results = namespace
        .query(Some(&query_vector), None, 10, Some(&filter))
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0.id, 2); // Only Gaming Mouse matches
}

/// Test WAL recovery after simulated crash
#[tokio::test]
async fn test_wal_recovery() {
    let temp_dir = TempDir::new().unwrap();
    let storage_path = temp_dir.path().to_path_buf();

    // Phase 1: Create namespace and insert data
    {
        let storage = Arc::new(LocalStorage::new(&storage_path).unwrap());
        let manager = Arc::new(NamespaceManager::new(storage));

        let schema = Schema {
            vector_dim: 64,
            vector_metric: DistanceMetric::L2,
            attributes: HashMap::new(),
        };

        let namespace = manager
            .create_namespace("test".to_string(), schema)
            .await
            .unwrap();

        // Insert documents
        let docs = vec![
            Document {
                id: 1,
                vector: Some(vec![1.0; 64]),
                attributes: HashMap::new(),
            },
            Document {
                id: 2,
                vector: Some(vec![2.0; 64]),
                attributes: HashMap::new(),
            },
        ];

        namespace.upsert(docs).await.unwrap();
        // Namespace and manager dropped here (simulated crash)
    }

    // Phase 2: Reload namespace - WAL should be replayed
    {
        let storage = Arc::new(LocalStorage::new(&storage_path).unwrap());
        let manager = Arc::new(NamespaceManager::new(storage));

        let namespace = manager.get_namespace("test").await.unwrap();

        // Verify namespace was reloaded successfully
        let stats = namespace.stats().await;
        assert_eq!(stats.total_docs, 2);

        // Query should work now (indexes rebuilt on load)
        let query = vec![1.5; 64];
        let results = namespace.query(Some(&query), None, 10, None).await.unwrap();
        assert_eq!(results.len(), 2);
    }
}

/// Test with cache enabled
#[tokio::test]
async fn test_with_cache() {
    let temp_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();

    let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());

    let cache_config = CacheConfig {
        memory_size: 100 * 1024 * 1024,  // 100MB
        disk_size: 500 * 1024 * 1024,    // 500MB
        disk_path: cache_dir.path().to_string_lossy().to_string(),
    };

    let cache = Arc::new(CacheManager::new(cache_config).await.unwrap());
    let manager = Arc::new(NamespaceManager::with_cache(storage, cache));

    let schema = Schema {
        vector_dim: 64,
        vector_metric: DistanceMetric::L2,
        attributes: HashMap::new(),
    };

    let namespace = manager
        .create_namespace("cached_ns".to_string(), schema)
        .await
        .unwrap();

    // Insert documents
    let docs: Vec<Document> = (0..10)
        .map(|i| Document {
            id: i,
            vector: Some(vec![i as f32; 64]),
            attributes: HashMap::new(),
        })
        .collect();

    namespace.upsert(docs).await.unwrap();

    // First query (cache miss)
    let query = vec![5.0; 64];
    let results1 = namespace.query(Some(&query), None, 5, None).await.unwrap();
    assert_eq!(results1.len(), 5);

    // Second query (should hit cache)
    let results2 = namespace.query(Some(&query), None, 5, None).await.unwrap();
    assert_eq!(results2.len(), 5);
    assert_eq!(results1[0].0.id, results2[0].0.id); // Same results
}

/// Test compaction
#[tokio::test]
async fn test_compaction() {
    let temp_dir = TempDir::new().unwrap();
    let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());

    // Use compaction config with low threshold
    let compaction_config = CompactionConfig::new(
        3600, // interval doesn't matter for manual compaction
        4,    // trigger after 4 segments (we'll have 5)
        1000,
    );

    let manager = Arc::new(NamespaceManager::with_compaction_config(
        storage,
        None,
        compaction_config,
    ));

    let schema = Schema {
        vector_dim: 64,
        vector_metric: DistanceMetric::L2,
        attributes: HashMap::new(),
    };

    let namespace = manager
        .create_namespace("compact_test".to_string(), schema)
        .await
        .unwrap();

    // Insert documents one by one to create multiple segments
    for i in 0..5 {
        let doc = Document {
            id: i,
            vector: Some(vec![i as f32; 64]),
            attributes: HashMap::new(),
        };
        namespace.upsert(vec![doc]).await.unwrap();
    }

    // Should have 5 segments
    let segment_count = namespace.segment_count().await;
    assert_eq!(segment_count, 5);

    // Should need compaction (5 > 4)
    let config = CompactionConfig::new(3600, 4, 1000);
    assert!(namespace.should_compact_with_config(&config).await);

    // Perform compaction
    namespace.compact().await.unwrap();

    // Segment count should be reduced
    assert!(namespace.segment_count().await < 5);

    // Data should still be queryable
    let query = vec![2.0; 64];
    let results = namespace.query(Some(&query), None, 10, None).await.unwrap();
    assert_eq!(results.len(), 5); // All documents still present
}

/// Test namespace reload after restart
#[tokio::test]
async fn test_namespace_persistence() {
    let temp_dir = TempDir::new().unwrap();
    let storage_path = temp_dir.path().to_path_buf();

    // Phase 1: Create and populate
    {
        let storage = Arc::new(LocalStorage::new(&storage_path).unwrap());
        let manager = Arc::new(NamespaceManager::new(storage));

        let mut attributes = HashMap::new();
        attributes.insert(
            "name".to_string(),
            AttributeSchema {
                attr_type: AttributeType::String,
                indexed: false,
                full_text: FullTextConfig::Simple(true),
            },
        );

        let schema = Schema {
            vector_dim: 64,
            vector_metric: DistanceMetric::L2,
            attributes,
        };

        let namespace = manager
            .create_namespace("persistent".to_string(), schema)
            .await
            .unwrap();

        let docs = vec![Document {
            id: 1,
            vector: Some(vec![1.0; 64]),
            attributes: {
                let mut attrs = HashMap::new();
                attrs.insert("name".to_string(), AttributeValue::String("Alice".to_string()));
                attrs
            },
        }];

        namespace.upsert(docs).await.unwrap();
    }

    // Phase 2: Reload and verify
    {
        let storage = Arc::new(LocalStorage::new(&storage_path).unwrap());
        let manager = Arc::new(NamespaceManager::new(storage));

        let namespace = manager.get_namespace("persistent").await.unwrap();

        // Verify schema is preserved
        let schema = namespace.schema().await;
        assert_eq!(schema.vector_dim, 64);
        assert!(schema.attributes.contains_key("name"));

        // Verify data is preserved
        let stats = namespace.stats().await;
        assert_eq!(stats.total_docs, 1);

        // Query should work now (indexes rebuilt on load)
        let query = vec![1.0; 64];
        let results = namespace.query(Some(&query), None, 10, None).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.id, 1);
    }
}

/// Test multi-field full-text search
#[tokio::test]
async fn test_multi_field_fulltext() {
    let temp_dir = TempDir::new().unwrap();
    let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());
    let manager = Arc::new(NamespaceManager::new(storage));

    let mut attributes = HashMap::new();
    attributes.insert(
        "title".to_string(),
        AttributeSchema {
            attr_type: AttributeType::String,
            indexed: false,
            full_text: FullTextConfig::Simple(true),
        },
    );
    attributes.insert(
        "description".to_string(),
        AttributeSchema {
            attr_type: AttributeType::String,
            indexed: false,
            full_text: FullTextConfig::Simple(true),
        },
    );

    let schema = Schema {
        vector_dim: 64,
        vector_metric: DistanceMetric::L2,
        attributes,
    };

    let namespace = manager
        .create_namespace("multifield".to_string(), schema)
        .await
        .unwrap();

    let docs = vec![Document {
        id: 1,
        vector: Some(vec![1.0; 64]),
        attributes: {
            let mut attrs = HashMap::new();
            attrs.insert(
                "title".to_string(),
                AttributeValue::String("Rust Programming".to_string()),
            );
            attrs.insert(
                "description".to_string(),
                AttributeValue::String("Learn Rust language".to_string()),
            );
            attrs
        },
    }];

    namespace.upsert(docs).await.unwrap();

    // Search across multiple fields
    let mut weights = HashMap::new();
    weights.insert("title".to_string(), 2.0);
    weights.insert("description".to_string(), 1.0);

    let ft_query = FullTextQuery::Multi {
        fields: vec!["title".to_string(), "description".to_string()],
        query: "rust language".to_string(),
        weights,
    };

    let results = namespace.query(None, Some(&ft_query), 10, None).await.unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].0.id, 1);
}
