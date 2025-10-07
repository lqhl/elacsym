//! Multi-node integration tests
//!
//! Tests distributed functionality with multiple indexer and query nodes.

use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;

use elacsym::namespace::{NamespaceManager, WalConfig};
use elacsym::sharding::{IndexerCluster, NodeConfig};
use elacsym::storage::local::LocalStorage;
use elacsym::types::{AttributeSchema, AttributeType, DistanceMetric, Document, Schema};

/// Test cluster setup with 3 indexer nodes
struct TestCluster {
    _storage_dir: TempDir,
    _storage: Arc<dyn elacsym::storage::StorageBackend>,
    indexers: Vec<(Arc<NamespaceManager>, Arc<IndexerCluster>)>,
    query_node: Arc<NamespaceManager>,
}

impl TestCluster {
    async fn new(num_indexers: usize) -> Self {
        let storage_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(storage_dir.path()).unwrap());
        let wal_root = storage_dir.path().join("wal");

        // Create indexer nodes
        let mut indexers = Vec::new();
        let node_ids: Vec<String> = (0..num_indexers)
            .map(|i| format!("indexer-{}", i))
            .collect();

        for i in 0..num_indexers {
            let node_id = format!("indexer-{}", i);
            let config = NodeConfig::new(node_id.clone(), num_indexers, i);
            let cluster = Arc::new(IndexerCluster::new(config, node_ids.clone()));

            let manager = Arc::new(NamespaceManager::new(
                storage.clone(),
                WalConfig::local(wal_root.clone()),
                node_id,
            ));

            indexers.push((manager, cluster));
        }

        // Create query node (can handle any namespace)
        let query_node = Arc::new(NamespaceManager::new(
            storage.clone(),
            WalConfig::local(wal_root.clone()),
            "query-node-1".to_string(),
        ));
        query_node.set_compaction_enabled(false);

        Self {
            _storage_dir: storage_dir,
            _storage: storage,
            indexers,
            query_node,
        }
    }

    /// Get the indexer responsible for a namespace
    fn get_indexer_for_namespace(&self, namespace: &str) -> &Arc<NamespaceManager> {
        for (manager, cluster) in &self.indexers {
            if cluster.should_handle(namespace) {
                return manager;
            }
        }
        panic!("No indexer found for namespace: {}", namespace);
    }
}

#[tokio::test]
async fn test_namespace_sharding() {
    // Create a 3-node cluster
    let cluster = TestCluster::new(3).await;

    // Create test schema
    let schema = Schema {
        vector_dim: 64,
        vector_metric: DistanceMetric::L2,
        attributes: {
            let mut attrs = HashMap::new();
            attrs.insert(
                "title".to_string(),
                AttributeSchema {
                    attr_type: AttributeType::String,
                    indexed: false,
                    full_text: elacsym::types::FullTextConfig::Simple(true),
                },
            );
            attrs
        },
    };

    // Create multiple namespaces
    let namespaces = vec!["ns_alpha", "ns_beta", "ns_gamma", "ns_delta", "ns_epsilon"];

    // Verify each namespace is assigned to correct indexer
    for ns_name in &namespaces {
        let indexer = cluster.get_indexer_for_namespace(ns_name);

        // Create namespace on the correct indexer
        let _ns = indexer
            .create_namespace(ns_name.to_string(), schema.clone())
            .await
            .unwrap();

        println!(
            "Namespace '{}' created on indexer '{}'",
            ns_name,
            indexer.node_id()
        );
    }

    // Verify distribution is relatively fair
    let mut distribution: HashMap<String, usize> = HashMap::new();
    for ns_name in &namespaces {
        let indexer = cluster.get_indexer_for_namespace(ns_name);
        *distribution
            .entry(indexer.node_id().to_string())
            .or_insert(0) += 1;
    }

    println!("Distribution: {:?}", distribution);

    // With 5 namespaces and 3 indexers, not all indexers may be used
    // Just verify that at least 2 indexers are used (fair distribution)
    assert!(
        distribution.len() >= 2,
        "Distribution should use at least 2 indexers"
    );

    // Verify no single indexer handles all namespaces
    for count in distribution.values() {
        assert!(
            *count < namespaces.len(),
            "No single indexer should handle all namespaces"
        );
    }
}

#[tokio::test]
async fn test_write_and_query_across_nodes() {
    let cluster = TestCluster::new(3).await;

    let schema = Schema {
        vector_dim: 64,
        vector_metric: DistanceMetric::L2,
        attributes: HashMap::new(),
    };

    // Create namespace on correct indexer
    let ns_name = "test_ns";
    let indexer = cluster.get_indexer_for_namespace(ns_name);

    let ns = indexer
        .create_namespace(ns_name.to_string(), schema)
        .await
        .unwrap();

    // Insert documents via indexer
    let documents = vec![
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

    ns.upsert(documents).await.unwrap();

    // Query from query node
    let query_ns = cluster.query_node.get_namespace(ns_name).await.unwrap();

    let query_vec = vec![1.5; 64];
    let results = query_ns
        .query(Some(&query_vec), None, 10, None)
        .await
        .unwrap();

    assert_eq!(results.len(), 2);
    println!("Query results: {} documents found", results.len());
}

#[tokio::test]
async fn test_query_node_does_not_run_compaction() {
    let cluster = TestCluster::new(2).await;

    let schema = Schema {
        vector_dim: 16,
        vector_metric: DistanceMetric::L2,
        attributes: HashMap::new(),
    };

    let ns_name = "no_compaction";
    let indexer = cluster.get_indexer_for_namespace(ns_name);
    indexer
        .create_namespace(ns_name.to_string(), schema.clone())
        .await
        .unwrap();

    // Loading through the query node should not register a compaction manager
    cluster
        .query_node
        .get_namespace(ns_name)
        .await
        .expect("query node should load namespace");

    assert!(
        !cluster.query_node.has_compaction_manager(ns_name).await,
        "query nodes must not start compaction managers"
    );
}

#[tokio::test]
async fn test_multiple_namespaces_parallel_writes() {
    let cluster = TestCluster::new(3).await;

    let schema = Schema {
        vector_dim: 32,
        vector_metric: DistanceMetric::L2,
        attributes: HashMap::new(),
    };

    // Create multiple namespaces in parallel
    let namespaces = vec!["ns1", "ns2", "ns3", "ns4", "ns5", "ns6"];

    let mut handles = vec![];

    for ns_name in namespaces {
        let indexer = cluster.get_indexer_for_namespace(ns_name).clone();
        let schema_clone = schema.clone();

        let handle = tokio::spawn(async move {
            // Create namespace
            let ns = indexer
                .create_namespace(ns_name.to_string(), schema_clone)
                .await
                .unwrap();

            // Insert documents
            for i in 0..10 {
                let doc = Document {
                    id: i,
                    vector: Some(vec![i as f32; 32]),
                    attributes: HashMap::new(),
                };

                ns.upsert(vec![doc]).await.unwrap();
            }

            ns_name.to_string()
        });

        handles.push(handle);
    }

    // Wait for all writes to complete
    let results: Vec<_> = futures::future::join_all(handles).await;
    assert_eq!(results.len(), 6);

    println!(
        "Successfully wrote to {} namespaces in parallel",
        results.len()
    );
}

#[tokio::test]
async fn test_wrong_indexer_detection() {
    let cluster = TestCluster::new(3).await;

    let schema = Schema {
        vector_dim: 32,
        vector_metric: DistanceMetric::L2,
        attributes: HashMap::new(),
    };

    let ns_name = "test_namespace";

    // Find which indexer should handle this namespace
    let correct_indexer = cluster.get_indexer_for_namespace(ns_name);
    let correct_node_id = correct_indexer.node_id().to_string();

    // Try to create on wrong indexers
    for (indexer, cluster_config) in &cluster.indexers {
        if indexer.node_id() == correct_node_id {
            // This is the correct indexer - should succeed
            let result = indexer
                .create_namespace(ns_name.to_string(), schema.clone())
                .await;
            assert!(
                result.is_ok(),
                "Correct indexer should successfully create namespace"
            );
        } else {
            // This is the wrong indexer - should detect it
            assert!(
                !cluster_config.should_handle(ns_name),
                "Wrong indexer should not claim to handle this namespace"
            );
        }
    }

    println!(
        "Namespace '{}' correctly assigned to indexer '{}'",
        ns_name, correct_node_id
    );
}

#[tokio::test]
async fn test_consistent_routing() {
    let cluster = TestCluster::new(3).await;

    // Test that namespace routing is deterministic
    let ns_name = "consistent_test";

    let indexer1 = cluster.get_indexer_for_namespace(ns_name);
    let indexer2 = cluster.get_indexer_for_namespace(ns_name);

    assert_eq!(
        indexer1.node_id(),
        indexer2.node_id(),
        "Namespace should always route to same indexer"
    );

    // Test across multiple calls
    for _ in 0..100 {
        let indexer = cluster.get_indexer_for_namespace(ns_name);
        assert_eq!(
            indexer.node_id(),
            indexer1.node_id(),
            "Routing must be consistent"
        );
    }

    println!("Routing consistency verified for 100 iterations");
}

#[tokio::test]
async fn test_cluster_expansion_simulation() {
    // Simulate expanding from 2 to 3 indexers

    // Original 2-node cluster
    let cluster_2 = TestCluster::new(2).await;
    let schema = Schema {
        vector_dim: 32,
        vector_metric: DistanceMetric::L2,
        attributes: HashMap::new(),
    };

    let ns_name = "expansion_test";

    // Create namespace on 2-node cluster
    let indexer_2node = cluster_2.get_indexer_for_namespace(ns_name);
    let node_id_2 = indexer_2node.node_id().to_string();

    indexer_2node
        .create_namespace(ns_name.to_string(), schema.clone())
        .await
        .unwrap();

    println!("In 2-node cluster: namespace assigned to '{}'", node_id_2);

    // Expand to 3-node cluster
    let cluster_3 = TestCluster::new(3).await;
    let indexer_3node = cluster_3.get_indexer_for_namespace(ns_name);
    let node_id_3 = indexer_3node.node_id().to_string();

    println!("In 3-node cluster: namespace assigned to '{}'", node_id_3);

    // Namespace might move to different node (this is expected)
    // In production, this would require manual migration
    if node_id_2 != node_id_3 {
        println!(
            "⚠️  Namespace moved from '{}' to '{}' after expansion (manual migration needed)",
            node_id_2, node_id_3
        );
    } else {
        println!("✓ Namespace stayed on same node after expansion");
    }
}
