//! S3-based metadata storage - no external dependencies

use async_trait::async_trait;
use elacsym_core::{Error, PartitionId, Result, SlabId, VectorId};
use object_store::{path::Path, ObjectStore};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info};

use crate::{Metadata, MetadataStore, NamespaceMetadata, VectorMapping};

/// S3-based metadata store
/// All metadata stored as JSON files in S3:
/// - metadata/namespaces/{namespace}.json
/// - metadata/partitions/{namespace}/{partition_id}.json
/// - metadata/vectors/{namespace}/{vector_id_prefix}/{vector_id}.json
pub struct S3MetadataStore {
    store: Arc<dyn ObjectStore>,
    base_path: String,
}

/// Partition metadata stored in S3
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionMetadata {
    pub partition_id: PartitionId,
    pub vector_count: usize,
    pub slab_ids: Vec<SlabId>,
    pub centroid: Vec<f32>,
}

impl S3MetadataStore {
    pub fn new(store: Arc<dyn ObjectStore>, base_path: String) -> Self {
        Self { store, base_path }
    }

    /// Get path for namespace metadata
    fn namespace_path(&self, namespace: &str) -> Path {
        Path::from(format!("{}/namespaces/{}.json", self.base_path, namespace))
    }

    /// Get path for partition metadata
    fn partition_path(&self, namespace: &str, partition_id: PartitionId) -> Path {
        Path::from(format!(
            "{}/partitions/{}/{}.json",
            self.base_path, namespace, partition_id
        ))
    }

    /// Get path for vector mapping
    fn vector_path(&self, namespace: &str, vector_id: &VectorId) -> Path {
        // Use prefix sharding to avoid hot spots
        let id_str = vector_id.as_str();
        let prefix = if id_str.len() >= 2 {
            &id_str[0..2]
        } else {
            "00"
        };

        Path::from(format!(
            "{}/vectors/{}/{}/{}.json",
            self.base_path, namespace, prefix, vector_id
        ))
    }

    /// List all partitions for a namespace
    async fn list_partitions(&self, namespace: &str) -> Result<Vec<PartitionId>> {
        let prefix = Path::from(format!("{}/partitions/{}/", self.base_path, namespace));

        let objects = self.store.list(Some(&prefix)).await
            .map_err(|e| Error::Metadata(format!("Failed to list partitions: {}", e)))?;

        let mut partition_ids = Vec::new();
        for meta in objects {
            if let Some(filename) = meta.location.filename() {
                if let Some(id_str) = filename.strip_suffix(".json") {
                    if let Ok(id) = id_str.parse::<PartitionId>() {
                        partition_ids.push(id);
                    }
                }
            }
        }

        Ok(partition_ids)
    }

    /// Update partition metadata
    pub async fn update_partition(
        &self,
        namespace: &str,
        partition_metadata: PartitionMetadata,
    ) -> Result<()> {
        let path = self.partition_path(namespace, partition_metadata.partition_id);
        let data = serde_json::to_vec_pretty(&partition_metadata)
            .map_err(|e| Error::Serialization(format!("Failed to serialize partition: {}", e)))?;

        self.store
            .put(&path, data.into())
            .await
            .map_err(|e| Error::Metadata(format!("Failed to store partition metadata: {}", e)))?;

        Ok(())
    }

    /// Get partition metadata
    pub async fn get_partition(
        &self,
        namespace: &str,
        partition_id: PartitionId,
    ) -> Result<Option<PartitionMetadata>> {
        let path = self.partition_path(namespace, partition_id);

        match self.store.get(&path).await {
            Ok(result) => {
                let data = result.bytes().await
                    .map_err(|e| Error::Metadata(format!("Failed to read partition: {}", e)))?;

                let metadata: PartitionMetadata = serde_json::from_slice(&data)
                    .map_err(|e| Error::Serialization(format!("Failed to deserialize partition: {}", e)))?;

                Ok(Some(metadata))
            }
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(Error::Metadata(format!("Failed to get partition: {}", e))),
        }
    }
}

impl MetadataStore for S3MetadataStore {
    fn get(&self, _key: &[u8]) -> Result<Option<Vec<u8>>> {
        // Not used in S3-based implementation
        unimplemented!("S3MetadataStore uses async methods")
    }

    fn put(&self, _key: &[u8], _value: &[u8]) -> Result<()> {
        // Not used in S3-based implementation
        unimplemented!("S3MetadataStore uses async methods")
    }

    fn delete(&self, _key: &[u8]) -> Result<()> {
        // Not used in S3-based implementation
        unimplemented!("S3MetadataStore uses async methods")
    }

    fn prefix_scan(&self, _prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        // Not used in S3-based implementation
        unimplemented!("S3MetadataStore uses async methods")
    }
}

/// S3-based metadata service
pub struct S3MetadataService {
    s3_store: Arc<S3MetadataStore>,
}

impl S3MetadataService {
    pub fn new(s3_store: Arc<S3MetadataStore>) -> Self {
        Self { s3_store }
    }

    /// Create or update namespace
    pub async fn upsert_namespace(&self, metadata: NamespaceMetadata) -> Result<()> {
        let path = self.s3_store.namespace_path(&metadata.name);
        let data = serde_json::to_vec_pretty(&metadata)
            .map_err(|e| Error::Serialization(format!("Failed to serialize namespace: {}", e)))?;

        self.s3_store
            .store
            .put(&path, data.into())
            .await
            .map_err(|e| Error::Metadata(format!("Failed to store namespace: {}", e)))?;

        info!("Created/updated namespace: {}", metadata.name);

        Ok(())
    }

    /// Get namespace metadata
    pub async fn get_namespace(&self, namespace: &str) -> Result<Option<NamespaceMetadata>> {
        let path = self.s3_store.namespace_path(namespace);

        match self.s3_store.store.get(&path).await {
            Ok(result) => {
                let data = result.bytes().await
                    .map_err(|e| Error::Metadata(format!("Failed to read namespace: {}", e)))?;

                let metadata: NamespaceMetadata = serde_json::from_slice(&data)
                    .map_err(|e| Error::Serialization(format!("Failed to deserialize namespace: {}", e)))?;

                Ok(Some(metadata))
            }
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(Error::Metadata(format!("Failed to get namespace: {}", e))),
        }
    }

    /// List all namespaces
    pub async fn list_namespaces(&self) -> Result<Vec<String>> {
        let prefix = Path::from(format!("{}/namespaces/", self.s3_store.base_path));

        let objects = self.s3_store.store.list(Some(&prefix)).await
            .map_err(|e| Error::Metadata(format!("Failed to list namespaces: {}", e)))?;

        let namespaces = objects
            .into_iter()
            .filter_map(|meta| {
                meta.location
                    .filename()
                    .and_then(|f| f.strip_suffix(".json"))
                    .map(|s| s.to_string())
            })
            .collect();

        Ok(namespaces)
    }

    /// Register slab to partition
    pub async fn register_slab(
        &self,
        namespace: &str,
        partition_id: PartitionId,
        slab_id: SlabId,
    ) -> Result<()> {
        // Get existing partition metadata
        let mut partition = self
            .s3_store
            .get_partition(namespace, partition_id)
            .await?
            .unwrap_or_else(|| PartitionMetadata {
                partition_id,
                vector_count: 0,
                slab_ids: Vec::new(),
                centroid: Vec::new(),
            });

        // Add slab if not already present
        if !partition.slab_ids.contains(&slab_id) {
            partition.slab_ids.push(slab_id);
        }

        // Update partition metadata
        self.s3_store
            .update_partition(namespace, partition)
            .await?;

        Ok(())
    }

    /// Get all slabs for a partition
    pub async fn get_partition_slabs(
        &self,
        namespace: &str,
        partition_id: PartitionId,
    ) -> Result<Vec<SlabId>> {
        match self.s3_store.get_partition(namespace, partition_id).await? {
            Some(partition) => Ok(partition.slab_ids),
            None => Ok(Vec::new()),
        }
    }

    /// Map vector to partition (batch operation for efficiency)
    pub async fn map_vectors_batch(
        &self,
        namespace: &str,
        mappings: Vec<VectorMapping>,
    ) -> Result<()> {
        // In S3-only design, we store minimal vector metadata
        // The partition info is in the slab itself
        // We can skip individual vector mappings for efficiency

        // Optionally, store a mapping file for reverse lookup
        debug!(
            "Batch mapping {} vectors in namespace {}",
            mappings.len(),
            namespace
        );

        Ok(())
    }

    /// Get all partitions for a namespace
    pub async fn list_partitions(&self, namespace: &str) -> Result<Vec<PartitionId>> {
        self.s3_store.list_partitions(namespace).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_s3_metadata_namespace() {
        let store = Arc::new(object_store::memory::InMemory::new());
        let s3_store = Arc::new(S3MetadataStore::new(store, "metadata".to_string()));
        let service = S3MetadataService::new(s3_store);

        let ns = NamespaceMetadata {
            name: "test".to_string(),
            dimension: 128,
            vector_count: 0,
            partition_count: 0,
        };

        service.upsert_namespace(ns.clone()).await.unwrap();

        let retrieved = service.get_namespace("test").await.unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().name, "test");

        let namespaces = service.list_namespaces().await.unwrap();
        assert!(namespaces.contains(&"test".to_string()));
    }
}
