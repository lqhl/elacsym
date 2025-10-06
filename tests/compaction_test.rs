use elacsym::namespace::Namespace;
use elacsym::storage::local::LocalStorage;
use elacsym::types::{AttributeValue, DistanceMetric, Document, FullTextConfig, Schema};
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;

#[tokio::test]
async fn test_compaction_merges_segments() {
    // Create temporary directory for storage
    let temp_dir = TempDir::new().unwrap();
    let storage_path = temp_dir.path().to_path_buf();

    // Create schema
    let mut attributes = HashMap::new();
    attributes.insert(
        "title".to_string(),
        elacsym::types::AttributeSchema {
            attr_type: elacsym::types::AttributeType::String,
            indexed: false,
            full_text: FullTextConfig::Simple(false),
        },
    );

    let schema = Schema {
        vector_dim: 3,
        vector_metric: DistanceMetric::L2,
        attributes,
    };

    // Create namespace
    let storage = Arc::new(LocalStorage::new(storage_path.clone()).unwrap());
    let namespace = Namespace::create(
        "test_compaction".to_string(),
        schema.clone(),
        storage.clone(),
        None,
        "test-node".to_string(),
    )
    .await
    .unwrap();

    // Insert multiple small batches to create many segments
    for i in 0..5 {
        let documents = vec![
            Document {
                id: (i * 10 + 1) as u64,
                vector: Some(vec![0.1 * i as f32, 0.2, 0.3]),
                attributes: {
                    let mut map = HashMap::new();
                    map.insert(
                        "title".to_string(),
                        AttributeValue::String(format!("Doc {}", i * 10 + 1)),
                    );
                    map
                },
            },
            Document {
                id: (i * 10 + 2) as u64,
                vector: Some(vec![0.4, 0.5 * i as f32, 0.6]),
                attributes: {
                    let mut map = HashMap::new();
                    map.insert(
                        "title".to_string(),
                        AttributeValue::String(format!("Doc {}", i * 10 + 2)),
                    );
                    map
                },
            },
        ];

        namespace.upsert(documents).await.unwrap();
    }

    // Verify we have multiple segments
    let stats_before = namespace.stats().await;
    assert_eq!(
        stats_before.total_docs, 10,
        "Should have 10 total documents"
    );

    // Check segment count before compaction
    let segment_count_before = namespace.segment_count().await;
    assert_eq!(
        segment_count_before, 5,
        "Should have 5 segments before compaction"
    );

    // Run compaction
    namespace.compact().await.unwrap();

    // Check segment count after compaction
    let segment_count_after = namespace.segment_count().await;
    assert!(
        segment_count_after < segment_count_before,
        "Should have fewer segments after compaction"
    );

    // Verify all data is still accessible
    let query_vector = vec![0.1, 0.2, 0.3];
    let results = namespace
        .query(Some(&query_vector), None, 10, None)
        .await
        .unwrap();

    assert_eq!(
        results.len(),
        10,
        "Should still find all 10 documents after compaction"
    );

    // Verify stats are correct after compaction
    let stats_after = namespace.stats().await;
    assert_eq!(
        stats_after.total_docs, 10,
        "Total docs should remain 10 after compaction"
    );
}

#[tokio::test]
async fn test_compaction_with_full_text_index() {
    // Create temporary directory for storage
    let temp_dir = TempDir::new().unwrap();
    let storage_path = temp_dir.path().to_path_buf();

    // Create schema with full-text search
    let mut attributes = HashMap::new();
    attributes.insert(
        "content".to_string(),
        elacsym::types::AttributeSchema {
            attr_type: elacsym::types::AttributeType::String,
            indexed: false,
            full_text: FullTextConfig::Simple(true), // Enable full-text search
        },
    );

    let schema = Schema {
        vector_dim: 3,
        vector_metric: DistanceMetric::L2,
        attributes,
    };

    // Create namespace
    let storage = Arc::new(LocalStorage::new(storage_path).unwrap());
    let namespace = Namespace::create(
        "test_compaction_fulltext".to_string(),
        schema,
        storage,
        None,
        "test-node".to_string(),
    )
    .await
    .unwrap();

    // Insert documents with text
    for i in 0..3 {
        let documents = vec![
            Document {
                id: (i * 10 + 1) as u64,
                vector: Some(vec![0.1, 0.2, 0.3]),
                attributes: {
                    let mut map = HashMap::new();
                    map.insert(
                        "content".to_string(),
                        AttributeValue::String(format!("This is document {} about rust", i)),
                    );
                    map
                },
            },
            Document {
                id: (i * 10 + 2) as u64,
                vector: Some(vec![0.4, 0.5, 0.6]),
                attributes: {
                    let mut map = HashMap::new();
                    map.insert(
                        "content".to_string(),
                        AttributeValue::String(format!("Document {} describes databases", i)),
                    );
                    map
                },
            },
        ];

        namespace.upsert(documents).await.unwrap();
    }

    // Verify full-text search works before compaction
    let ft_query = elacsym::query::FullTextQuery::Single {
        field: "content".to_string(),
        query: "rust".to_string(),
        weight: 1.0,
    };

    let results_before = namespace
        .query(None, Some(&ft_query), 10, None)
        .await
        .unwrap();
    assert_eq!(
        results_before.len(),
        3,
        "Should find 3 documents with 'rust' before compaction"
    );

    // Run compaction
    namespace.compact().await.unwrap();

    // Verify full-text search still works after compaction
    let results_after = namespace
        .query(None, Some(&ft_query), 10, None)
        .await
        .unwrap();
    assert_eq!(
        results_after.len(),
        3,
        "Should still find 3 documents with 'rust' after compaction"
    );
}

#[tokio::test]
async fn test_should_compact_threshold() {
    // Create temporary directory for storage
    let temp_dir = TempDir::new().unwrap();
    let storage_path = temp_dir.path().to_path_buf();

    // Create simple schema
    let schema = Schema {
        vector_dim: 3,
        vector_metric: DistanceMetric::L2,
        attributes: HashMap::new(),
    };

    // Create namespace
    let storage = Arc::new(LocalStorage::new(storage_path).unwrap());
    let namespace = Namespace::create("test_threshold".to_string(), schema, storage, None, "test-node".to_string())
        .await
        .unwrap();

    // Initially should not need compaction
    assert!(
        !namespace.should_compact().await,
        "Should not need compaction with 0 segments"
    );

    // Insert one batch
    let documents = vec![Document {
        id: 1,
        vector: Some(vec![0.1, 0.2, 0.3]),
        attributes: HashMap::new(),
    }];

    namespace.upsert(documents).await.unwrap();

    // Still should not need compaction with only 1 segment
    assert!(
        !namespace.should_compact().await,
        "Should not need compaction with 1 segment"
    );

    // Insert many more batches to exceed threshold
    for i in 2..=101 {
        let documents = vec![Document {
            id: i,
            vector: Some(vec![0.1, 0.2, 0.3]),
            attributes: HashMap::new(),
        }];

        namespace.upsert(documents).await.unwrap();
    }

    // Now should need compaction (> 100 segments)
    assert!(
        namespace.should_compact().await,
        "Should need compaction with > 100 segments"
    );
}
