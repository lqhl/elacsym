//! Storage abstraction layer
//!
//! Provides unified interface for S3 and local filesystem storage

use async_trait::async_trait;
use bytes::Bytes;

use crate::Result;

pub mod local;
pub mod s3;

/// Storage backend trait
#[async_trait]
pub trait StorageBackend: Send + Sync {
    /// Read object from storage
    async fn get(&self, key: &str) -> Result<Bytes>;

    /// Write object to storage
    async fn put(&self, key: &str, data: Bytes) -> Result<()>;

    /// Delete object from storage
    async fn delete(&self, key: &str) -> Result<()>;

    /// Check if object exists
    async fn exists(&self, key: &str) -> Result<bool>;

    /// List objects with prefix
    async fn list(&self, prefix: &str) -> Result<Vec<String>>;

    /// Get object with byte range
    async fn get_range(&self, key: &str, start: u64, end: u64) -> Result<Bytes>;
}

/// Storage configuration
#[derive(Debug, Clone)]
pub enum StorageConfig {
    S3 {
        bucket: String,
        region: String,
        endpoint: Option<String>,
    },
    Local {
        root_path: String,
    },
}

/// Create storage backend from config
pub async fn create_storage(config: StorageConfig) -> Result<Box<dyn StorageBackend>> {
    match config {
        StorageConfig::S3 {
            bucket,
            region,
            endpoint,
        } => {
            let backend = s3::S3Storage::new(bucket, region, endpoint).await?;
            Ok(Box::new(backend))
        }
        StorageConfig::Local { root_path } => {
            let backend = local::LocalStorage::new(root_path)?;
            Ok(Box::new(backend))
        }
    }
}
