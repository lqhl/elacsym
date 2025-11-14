//! Blob storage implementation using object_store

use crate::SlabStorage;
use async_trait::async_trait;
use bytes::Bytes;
use elacsym_core::{Error, Result, Slab, SlabId};
use object_store::{path::Path, ObjectStore};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, info};

/// Configuration for blob storage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobStorageConfig {
    /// Base path/prefix for slabs
    pub base_path: String,

    /// Storage backend type
    pub backend: StorageBackend,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum StorageBackend {
    /// AWS S3
    S3 {
        bucket: String,
        region: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        endpoint: Option<String>,
    },
    /// Local filesystem (for testing)
    Local { path: String },
    /// Memory (for testing)
    Memory,
}

impl Default for BlobStorageConfig {
    fn default() -> Self {
        Self {
            base_path: "slabs".to_string(),
            backend: StorageBackend::Memory,
        }
    }
}

/// Blob storage implementation
pub struct BlobStorage {
    config: BlobStorageConfig,
    store: Arc<dyn ObjectStore>,
}

impl BlobStorage {
    pub async fn new(config: BlobStorageConfig) -> Result<Self> {
        let store = Self::create_store(&config).await?;

        Ok(Self {
            config,
            store: Arc::new(store),
        })
    }

    async fn create_store(config: &BlobStorageConfig) -> Result<Box<dyn ObjectStore>> {
        match &config.backend {
            StorageBackend::Memory => {
                info!("Using in-memory storage");
                Ok(Box::new(object_store::memory::InMemory::new()))
            }
            StorageBackend::Local { path } => {
                info!("Using local filesystem storage at: {}", path);
                Ok(Box::new(object_store::local::LocalFileSystem::new_with_prefix(path)?))
            }
            StorageBackend::S3 {
                bucket,
                region,
                endpoint,
            } => {
                info!("Using S3 storage: bucket={}, region={}", bucket, region);

                let mut builder = object_store::aws::AmazonS3Builder::new()
                    .with_bucket_name(bucket)
                    .with_region(region);

                if let Some(endpoint) = endpoint {
                    builder = builder.with_endpoint(endpoint);
                }

                let s3 = builder.build()?;
                Ok(Box::new(s3))
            }
        }
    }

    /// Get object path for a slab
    fn slab_path(&self, namespace: &str, id: &SlabId) -> Path {
        Path::from(format!("{}/{}/{}.slab", self.config.base_path, namespace, id))
    }

    /// Serialize and compress slab
    fn serialize_slab(&self, slab: &Slab) -> Result<Bytes> {
        let data = bincode::serialize(slab)
            .map_err(|e| Error::Serialization(format!("Slab serialization failed: {}", e)))?;

        Ok(Bytes::from(data))
    }

    /// Deserialize and decompress slab
    fn deserialize_slab(&self, data: &[u8]) -> Result<Slab> {
        let slab: Slab = bincode::deserialize(data)
            .map_err(|e| Error::Serialization(format!("Slab deserialization failed: {}", e)))?;

        // Verify checksum
        if !slab.verify_checksum() {
            return Err(Error::Storage(anyhow::anyhow!("Slab checksum verification failed")));
        }

        Ok(slab)
    }
}

#[async_trait]
impl SlabStorage for BlobStorage {
    async fn put_slab(&self, slab: &Slab) -> Result<()> {
        let path = self.slab_path(&slab.metadata.namespace, &slab.metadata.id);
        let data = self.serialize_slab(slab)?;

        debug!("Storing slab {} at path: {}", slab.metadata.id, path);

        self.store
            .put(&path, data.into())
            .await
            .map_err(|e| Error::Storage(anyhow::anyhow!("Failed to store slab: {}", e)))?;

        info!(
            "Stored slab {} ({} bytes)",
            slab.metadata.id,
            slab.size_bytes()
        );

        Ok(())
    }

    async fn get_slab(&self, id: &SlabId) -> Result<Option<Slab>> {
        // Note: We need namespace to construct path
        // This is a simplified implementation; in practice, metadata service would provide namespace
        // For now, we'll need to list and find
        Err(Error::Storage(anyhow::anyhow!(
            "get_slab requires namespace context - use metadata service"
        )))
    }

    async fn delete_slab(&self, id: &SlabId) -> Result<()> {
        // Similar issue - need namespace
        Err(Error::Storage(anyhow::anyhow!(
            "delete_slab requires namespace context - use metadata service"
        )))
    }

    async fn list_slabs(&self, namespace: &str) -> Result<Vec<SlabId>> {
        let prefix = Path::from(format!("{}/{}", self.config.base_path, namespace));

        debug!("Listing slabs for namespace: {}", namespace);

        let result = self
            .store
            .list(Some(&prefix))
            .await
            .map_err(|e| Error::Storage(anyhow::anyhow!("Failed to list slabs: {}", e)))?;

        let mut slab_ids = Vec::new();
        for meta in result {
            if let Some(filename) = meta.location.filename() {
                if let Some(id_str) = filename.strip_suffix(".slab") {
                    slab_ids.push(SlabId::new(id_str));
                }
            }
        }

        info!("Found {} slabs in namespace {}", slab_ids.len(), namespace);

        Ok(slab_ids)
    }

    async fn exists(&self, id: &SlabId) -> Result<bool> {
        // Need namespace context
        Err(Error::Storage(anyhow::anyhow!(
            "exists requires namespace context - use metadata service"
        )))
    }
}

impl BlobStorage {
    /// Get slab with namespace (extended API)
    pub async fn get_slab_with_namespace(&self, namespace: &str, id: &SlabId) -> Result<Option<Slab>> {
        let path = self.slab_path(namespace, id);

        debug!("Retrieving slab {} from path: {}", id, path);

        match self.store.get(&path).await {
            Ok(result) => {
                let data = result.bytes().await
                    .map_err(|e| Error::Storage(anyhow::anyhow!("Failed to read slab data: {}", e)))?;

                let slab = self.deserialize_slab(&data)?;

                info!("Retrieved slab {} ({} bytes)", id, slab.size_bytes());

                Ok(Some(slab))
            }
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(Error::Storage(anyhow::anyhow!("Failed to get slab: {}", e))),
        }
    }

    /// Delete slab with namespace (extended API)
    pub async fn delete_slab_with_namespace(&self, namespace: &str, id: &SlabId) -> Result<()> {
        let path = self.slab_path(namespace, id);

        debug!("Deleting slab {} at path: {}", id, path);

        self.store
            .delete(&path)
            .await
            .map_err(|e| Error::Storage(anyhow::anyhow!("Failed to delete slab: {}", e)))?;

        info!("Deleted slab {}", id);

        Ok(())
    }
}
