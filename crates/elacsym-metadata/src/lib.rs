//! elacsym-metadata: Metadata service for managing index metadata

pub mod store;
pub mod service;

pub use store::{MetadataStore, RocksDBStore};
pub use service::{MetadataService, NamespaceMetadata, VectorMapping};

use elacsym_core::{PartitionId, VectorId, SlabId};
use async_trait::async_trait;
use elacsym_core::Result;

/// Metadata interface
#[async_trait]
pub trait Metadata: Send + Sync {
    /// Get partition for a vector
    async fn get_vector_partition(&self, namespace: &str, id: &VectorId) -> Result<Option<PartitionId>>;

    /// Get slabs for a partition
    async fn get_partition_slabs(&self, namespace: &str, partition_id: PartitionId) -> Result<Vec<SlabId>>;

    /// Register a new slab
    async fn register_slab(&self, namespace: &str, partition_id: PartitionId, slab_id: SlabId) -> Result<()>;

    /// Get namespace metadata
    async fn get_namespace(&self, namespace: &str) -> Result<Option<NamespaceMetadata>>;
}
