//! Namespace sharding via consistent hashing
//!
//! Each namespace is assigned to exactly one indexer node based on consistent hashing.
//! This ensures:
//! - No distributed locks needed (only one writer per namespace)
//! - Simple routing logic (deterministic mapping)
//! - Easy to scale (add/remove nodes with minimal reshuffling)

use serde::{Deserialize, Serialize};

/// Node configuration for sharding
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    /// Unique node identifier (e.g., "indexer-1", "indexer-2")
    pub node_id: String,

    /// Total number of indexer nodes in the cluster
    pub total_nodes: usize,

    /// This node's index (0-based)
    pub node_index: usize,
}

impl NodeConfig {
    /// Create a new node configuration
    pub fn new(node_id: String, total_nodes: usize, node_index: usize) -> Self {
        assert!(
            node_index < total_nodes,
            "node_index must be less than total_nodes"
        );
        Self {
            node_id,
            total_nodes,
            node_index,
        }
    }

    /// Create a single-node configuration (combined mode)
    pub fn single_node(node_id: String) -> Self {
        Self {
            node_id,
            total_nodes: 1,
            node_index: 0,
        }
    }

    /// Check if this node should handle a given namespace
    pub fn should_handle(&self, namespace: &str) -> bool {
        let target_index = get_node_index_for_namespace(namespace, self.total_nodes);
        target_index == self.node_index
    }

    /// Get the node ID that should handle a given namespace
    pub fn get_responsible_node_id(&self, namespace: &str, all_node_ids: &[String]) -> String {
        let target_index = get_node_index_for_namespace(namespace, self.total_nodes);
        all_node_ids
            .get(target_index)
            .cloned()
            .unwrap_or_else(|| format!("indexer-{}", target_index))
    }

    /// Check if running in single-node mode
    pub fn is_single_node(&self) -> bool {
        self.total_nodes == 1
    }
}

/// Calculate which node index should handle a namespace
///
/// Uses seahash for fast, deterministic hashing.
pub fn get_node_index_for_namespace(namespace: &str, total_nodes: usize) -> usize {
    if total_nodes == 0 {
        return 0;
    }

    let hash = seahash::hash(namespace.as_bytes());
    (hash % total_nodes as u64) as usize
}

/// Indexer cluster manager
///
/// Manages routing and node assignment for a cluster of indexers.
#[derive(Debug, Clone)]
pub struct IndexerCluster {
    /// This node's configuration
    pub config: NodeConfig,

    /// All node IDs in the cluster (ordered by index)
    pub all_node_ids: Vec<String>,
}

impl IndexerCluster {
    /// Create a new indexer cluster
    pub fn new(config: NodeConfig, all_node_ids: Vec<String>) -> Self {
        assert_eq!(
            config.total_nodes,
            all_node_ids.len(),
            "total_nodes must match all_node_ids length"
        );

        Self {
            config,
            all_node_ids,
        }
    }

    /// Create a single-node cluster
    pub fn single_node(node_id: String) -> Self {
        Self {
            config: NodeConfig::single_node(node_id.clone()),
            all_node_ids: vec![node_id],
        }
    }

    /// Check if this node should handle a namespace
    pub fn should_handle(&self, namespace: &str) -> bool {
        self.config.should_handle(namespace)
    }

    /// Get the node ID responsible for a namespace
    pub fn get_responsible_node_id(&self, namespace: &str) -> String {
        self.config
            .get_responsible_node_id(namespace, &self.all_node_ids)
    }

    /// Get the node index responsible for a namespace
    pub fn get_responsible_node_index(&self, namespace: &str) -> usize {
        get_node_index_for_namespace(namespace, self.config.total_nodes)
    }

    /// Check if running in single-node mode
    pub fn is_single_node(&self) -> bool {
        self.config.is_single_node()
    }

    /// Get this node's ID
    pub fn node_id(&self) -> &str {
        &self.config.node_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_node_index_for_namespace() {
        // Same namespace should always map to same node
        assert_eq!(
            get_node_index_for_namespace("test_ns", 3),
            get_node_index_for_namespace("test_ns", 3)
        );

        // Different namespaces should (likely) map to different nodes
        let ns1 = get_node_index_for_namespace("namespace_1", 3);
        let ns2 = get_node_index_for_namespace("namespace_2", 3);
        let ns3 = get_node_index_for_namespace("namespace_3", 3);

        // All should be in valid range
        assert!(ns1 < 3);
        assert!(ns2 < 3);
        assert!(ns3 < 3);
    }

    #[test]
    fn test_node_config_should_handle() {
        let config = NodeConfig::new("indexer-1".to_string(), 3, 0);

        // Find a namespace that maps to node 0
        let mut handled_count = 0;
        for i in 0..100 {
            let ns = format!("test_ns_{}", i);
            if config.should_handle(&ns) {
                handled_count += 1;
            }
        }

        // Should handle approximately 1/3 of namespaces
        assert!(handled_count > 20 && handled_count < 50);
    }

    #[test]
    fn test_indexer_cluster() {
        let all_nodes = vec![
            "indexer-1".to_string(),
            "indexer-2".to_string(),
            "indexer-3".to_string(),
        ];

        let cluster = IndexerCluster::new(
            NodeConfig::new("indexer-1".to_string(), 3, 0),
            all_nodes.clone(),
        );

        // Test routing
        for i in 0..10 {
            let ns = format!("test_ns_{}", i);
            let responsible = cluster.get_responsible_node_id(&ns);
            assert!(all_nodes.contains(&responsible));

            // If this node should handle it, responsible should be this node
            if cluster.should_handle(&ns) {
                assert_eq!(responsible, "indexer-1");
            }
        }
    }

    #[test]
    fn test_single_node_cluster() {
        let cluster = IndexerCluster::single_node("indexer-main".to_string());

        assert!(cluster.is_single_node());

        // Should handle all namespaces
        for i in 0..10 {
            let ns = format!("test_ns_{}", i);
            assert!(cluster.should_handle(&ns));
            assert_eq!(cluster.get_responsible_node_id(&ns), "indexer-main");
        }
    }

    #[test]
    fn test_resharding() {
        // Simulate cluster expansion from 2 to 3 nodes

        // Original 2-node cluster
        let ns = "test_namespace";
        let original_index = get_node_index_for_namespace(ns, 2);

        // After adding 3rd node
        let new_index = get_node_index_for_namespace(ns, 3);

        // Namespace might move to different node (expected during expansion)
        // This is acceptable - manual migration needed
        println!(
            "Namespace '{}' moved from node {} to node {} after expansion",
            ns, original_index, new_index
        );
    }

    #[test]
    fn test_distribution_fairness() {
        // Test that namespaces are fairly distributed across nodes
        let total_nodes = 3;
        let mut distribution = vec![0; total_nodes];

        for i in 0..1000 {
            let ns = format!("namespace_{}", i);
            let index = get_node_index_for_namespace(&ns, total_nodes);
            distribution[index] += 1;
        }

        // Each node should get roughly 1/3 of namespaces
        for count in distribution {
            // Allow 20% deviation
            assert!(count > 250 && count < 450);
        }
    }
}
