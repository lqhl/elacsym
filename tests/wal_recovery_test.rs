use elacsym::namespace::Namespace;
use elacsym::storage::local::LocalStorage;
use elacsym::types::{AttributeValue, DistanceMetric, Document, FullTextConfig, Schema};
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;

#[tokio::test]
async fn test_wal_recovery_after_crash() {
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

    // Step 1: Create namespace and insert documents
    let namespace_name = "test_recovery".to_string();
    let documents = vec![
        Document {
            id: 1,
            vector: Some(vec![0.1, 0.2, 0.3]),
            attributes: {
                let mut map = HashMap::new();
                map.insert(
                    "title".to_string(),
                    AttributeValue::String("Doc 1".to_string()),
                );
                map
            },
        },
        Document {
            id: 2,
            vector: Some(vec![0.4, 0.5, 0.6]),
            attributes: {
                let mut map = HashMap::new();
                map.insert(
                    "title".to_string(),
                    AttributeValue::String("Doc 2".to_string()),
                );
                map
            },
        },
    ];

    // Create namespace
    let storage = Arc::new(LocalStorage::new(storage_path.clone()).unwrap());
    let namespace = Namespace::create(
        namespace_name.clone(),
        schema.clone(),
        storage.clone(),
        None,
    )
    .await
    .unwrap();

    // Manually write to WAL (simulating crash during upsert)
    // We'll use the WAL directly to simulate a crash scenario
    {
        use elacsym::wal::WalManager;

        let wal_dir = format!("wal/{}", namespace_name);
        let mut wal = WalManager::new(&wal_dir).await.unwrap();

        // Write operation to WAL
        wal.append(elacsym::wal::WalOperation::Upsert {
            documents: documents.clone(),
        })
        .await
        .unwrap();
        wal.sync().await.unwrap();

        // DO NOT truncate - simulating crash before commit completes
        drop(wal);
    }

    // Drop namespace (simulating crash/restart)
    drop(namespace);

    // Step 2: Reload namespace - WAL should be replayed
    let storage2 = Arc::new(LocalStorage::new(storage_path).unwrap());
    let namespace2 = Namespace::load(namespace_name, storage2, None)
        .await
        .unwrap();

    // Step 3: Verify data was recovered
    // Query for document 1
    let query_vector = vec![0.1, 0.2, 0.3];
    let results = namespace2
        .query(Some(&query_vector), None, 10, None)
        .await
        .unwrap();

    // Should find at least document 1
    assert!(
        results.len() >= 1,
        "Expected at least 1 result after WAL recovery, got {}",
        results.len()
    );

    // Verify the document was recovered
    let found_doc1 = results.iter().any(|(doc, _)| doc.id == 1);
    let found_doc2 = results.iter().any(|(doc, _)| doc.id == 2);

    assert!(found_doc1, "Document 1 should be recovered from WAL");
    assert!(found_doc2, "Document 2 should be recovered from WAL");
}

#[tokio::test]
async fn test_wal_empty_after_successful_upsert() {
    // Create temporary directory
    let temp_dir = TempDir::new().unwrap();
    let storage_path = temp_dir.path().to_path_buf();

    // Create schema
    let schema = Schema {
        vector_dim: 3,
        vector_metric: DistanceMetric::L2,
        attributes: HashMap::new(),
    };

    // Create namespace
    let storage = Arc::new(LocalStorage::new(storage_path.clone()).unwrap());
    let namespace = Namespace::create("test_wal_truncate".to_string(), schema, storage, None)
        .await
        .unwrap();

    // Upsert documents (should write WAL and then truncate)
    let documents = vec![Document {
        id: 1,
        vector: Some(vec![0.1, 0.2, 0.3]),
        attributes: HashMap::new(),
    }];

    namespace.upsert(documents).await.unwrap();

    // Verify WAL is empty after successful upsert
    // We can check by trying to load - if WAL had entries, they would be replayed
    // Since we just successfully upserted, WAL should be truncated and empty
    use elacsym::wal::WalManager;
    let wal_dir = "wal/test_wal_truncate";
    let wal = WalManager::new(&wal_dir).await.unwrap();
    let entries = wal.read_all().await.unwrap();
    assert_eq!(
        entries.len(),
        0,
        "WAL should be empty after successful upsert"
    );
}
