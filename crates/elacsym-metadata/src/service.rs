//! Metadata service implementation

use crate::{Metadata, MetadataStore};
use async_trait::async_trait;
use elacsym_core::{Error, PartitionId, Result, SlabId, VectorId, Dimension};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Namespace metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamespaceMetadata {
    pub name: String,
    pub dimension: Dimension,
    pub vector_count: usize,
    pub partition_count: usize,
}

/// Vector to partition mapping
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorMapping {
    pub vector_id: VectorId,
    pub partition_id: PartitionId,
}

/// Metadata service
pub struct MetadataService<S: MetadataStore> {
    store: Arc<S>,
}

impl<S: MetadataStore> MetadataService<S> {
    pub fn new(store: Arc<S>) -> Self {
        Self { store }
    }

    /// Key for namespace metadata
    fn namespace_key(namespace: &str) -> Vec<u8> {
        format!("ns:{}", namespace).into_bytes()
    }

    /// Key for vector mapping
    fn vector_key(namespace: &str, vector_id: &VectorId) -> Vec<u8> {
        format!("vec:{}:{}", namespace, vector_id).into_bytes()
    }

    /// Key for partition slabs
    fn partition_key(namespace: &str, partition_id: PartitionId) -> Vec<u8> {
        format!("part:{}:{}", namespace, partition_id).into_bytes()
    }

    /// Prefix for all partitions in a namespace
    fn partition_prefix(namespace: &str) -> Vec<u8> {
        format!("part:{}:", namespace).into_bytes()
    }
}

#[async_trait]
impl<S: MetadataStore> Metadata for MetadataService<S> {
    async fn get_vector_partition(&self, namespace: &str, id: &VectorId) -> Result<Option<PartitionId>> {
        let key = Self::vector_key(namespace, id);

        if let Some(value) = self.store.get(&key)? {
            let partition_id: PartitionId = bincode::deserialize(&value)
                .map_err(|e| Error::Serialization(format!("Failed to deserialize partition ID: {}", e)))?;
            Ok(Some(partition_id))
        } else {
            Ok(None)
        }
    }

    async fn get_partition_slabs(&self, namespace: &str, partition_id: PartitionId) -> Result<Vec<SlabId>> {
        let key = Self::partition_key(namespace, partition_id);

        if let Some(value) = self.store.get(&key)? {
            let slab_ids: Vec<SlabId> = bincode::deserialize(&value)
                .map_err(|e| Error::Serialization(format!("Failed to deserialize slab IDs: {}", e)))?;
            Ok(slab_ids)
        } else {
            Ok(Vec::new())
        }
    }

    async fn register_slab(&self, namespace: &str, partition_id: PartitionId, slab_id: SlabId) -> Result<()> {
        let key = Self::partition_key(namespace, partition_id);

        // Get existing slabs
        let mut slab_ids = if let Some(value) = self.store.get(&key)? {
            bincode::deserialize(&value)
                .map_err(|e| Error::Serialization(format!("Failed to deserialize slab IDs: {}", e)))?
        } else {
            Vec::new()
        };

        // Add new slab if not already present
        if !slab_ids.iter().any(|id: &SlabId| id == &slab_id) {
            slab_ids.push(slab_id);
        }

        // Store updated list
        let value = bincode::serialize(&slab_ids)
            .map_err(|e| Error::Serialization(format!("Failed to serialize slab IDs: {}", e)))?;

        self.store.put(&key, &value)?;

        Ok(())
    }

    async fn get_namespace(&self, namespace: &str) -> Result<Option<NamespaceMetadata>> {
        let key = Self::namespace_key(namespace);

        if let Some(value) = self.store.get(&key)? {
            let metadata: NamespaceMetadata = bincode::deserialize(&value)
                .map_err(|e| Error::Serialization(format!("Failed to deserialize namespace metadata: {}", e)))?;
            Ok(Some(metadata))
        } else {
            Ok(None)
        }
    }
}

impl<S: MetadataStore> MetadataService<S> {
    /// Create or update namespace
    pub async fn upsert_namespace(&self, metadata: NamespaceMetadata) -> Result<()> {
        let key = Self::namespace_key(&metadata.name);
        let value = bincode::serialize(&metadata)
            .map_err(|e| Error::Serialization(format!("Failed to serialize namespace metadata: {}", e)))?;

        self.store.put(&key, &value)?;

        Ok(())
    }

    /// Map vector to partition
    pub async fn map_vector(&self, namespace: &str, vector_id: VectorId, partition_id: PartitionId) -> Result<()> {
        let key = Self::vector_key(namespace, &vector_id);
        let value = bincode::serialize(&partition_id)
            .map_err(|e| Error::Serialization(format!("Failed to serialize partition ID: {}", e)))?;

        self.store.put(&key, &value)?;

        Ok(())
    }

    /// List all namespaces
    pub async fn list_namespaces(&self) -> Result<Vec<String>> {
        let prefix = b"ns:";
        let results = self.store.prefix_scan(prefix)?;

        let namespaces = results
            .into_iter()
            .filter_map(|(key, _)| {
                String::from_utf8(key)
                    .ok()
                    .and_then(|s| s.strip_prefix("ns:").map(|s| s.to_string()))
            })
            .collect();

        Ok(namespaces)
    }
}
