//! API server state with sharding support

use std::sync::Arc;

use crate::namespace::NamespaceManager;
use crate::sharding::IndexerCluster;

/// API server state
#[derive(Clone)]
pub struct AppState {
    /// Namespace manager
    pub manager: Arc<NamespaceManager>,

    /// Indexer cluster configuration (optional for single-node mode)
    pub cluster: Option<Arc<IndexerCluster>>,

    /// Role for this node
    role: NodeRole,
}

impl AppState {
    /// Create state for single-node mode
    pub fn single_node(manager: Arc<NamespaceManager>) -> Self {
        let node_id = manager.node_id().to_string();
        let cluster = Arc::new(IndexerCluster::single_node(node_id));

        Self {
            manager,
            cluster: Some(cluster),
            role: NodeRole::Indexer,
        }
    }

    /// Create state for multi-node mode
    pub fn multi_node(
        manager: Arc<NamespaceManager>,
        cluster: Arc<IndexerCluster>,
        role: NodeRole,
    ) -> Self {
        Self {
            manager,
            cluster: Some(cluster),
            role,
        }
    }

    /// Check if this node should handle a namespace
    pub fn should_handle(&self, namespace: &str) -> bool {
        match self.role {
            NodeRole::Indexer => match &self.cluster {
                Some(cluster) => cluster.should_handle(namespace),
                None => true,
            },
            NodeRole::Query => false,
        }
    }

    /// Get the responsible node ID for a namespace
    pub fn get_responsible_node_id(&self, namespace: &str) -> Option<String> {
        self.cluster
            .as_ref()
            .map(|c| c.get_responsible_node_id(namespace))
    }

    /// Get this node's ID
    pub fn node_id(&self) -> &str {
        self.manager.node_id()
    }

    /// Return this node's role.
    pub fn role(&self) -> NodeRole {
        self.role
    }
}

/// Role of a node in the cluster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeRole {
    Indexer,
    Query,
}
