//! Cache layer using foyer
//!
//! Implements a two-tier cache:
//! - Memory cache: For manifests and indexes (hot data)
//! - Disk cache: For segment data (warm data)

use bytes::Bytes;
use foyer::{Cache, CacheBuilder, DirectFsDeviceOptions};
use std::sync::Arc;

use crate::Result;

/// Cache configuration
#[derive(Clone)]
pub struct CacheConfig {
    pub memory_size: usize,
    pub disk_size: usize,
    pub disk_path: String,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            memory_size: 4 * 1024 * 1024 * 1024,      // 4GB
            disk_size: 100 * 1024 * 1024 * 1024,      // 100GB
            disk_path: "/tmp/elacsym-cache".to_string(),
        }
    }
}

/// Cache manager with hybrid memory + disk caching
pub struct CacheManager {
    cache: Arc<Cache<String, Bytes>>,
}

impl CacheManager {
    /// Create a new cache manager
    pub async fn new(config: CacheConfig) -> Result<Self> {
        // Build hybrid cache with memory and disk layers
        let cache = CacheBuilder::new(config.memory_size)
            .with_shards(16) // Shard for concurrency
            .storage()
            .with_capacity(config.disk_size)
            .with_device_options(DirectFsDeviceOptions::new(&config.disk_path))
            .build()
            .await
            .map_err(|e| crate::Error::internal(format!("Failed to build cache: {}", e)))?;

        Ok(Self {
            cache: Arc::new(cache),
        })
    }

    /// Get value from cache
    pub async fn get(&self, key: &str) -> Option<Bytes> {
        self.cache.get(&key.to_string()).await
    }

    /// Put value into cache
    pub async fn put(&self, key: String, value: Bytes) {
        self.cache.insert(key, value).await;
    }

    /// Remove value from cache
    pub async fn remove(&self, key: &str) {
        self.cache.remove(&key.to_string()).await;
    }

    /// Get or fetch value (with callback for cache miss)
    pub async fn get_or_fetch<F, Fut>(&self, key: &str, fetch_fn: F) -> Result<Bytes>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Bytes>>,
    {
        // Try cache first
        if let Some(data) = self.get(key).await {
            return Ok(data);
        }

        // Cache miss - fetch from source
        let data = fetch_fn().await?;

        // Store in cache for next time
        self.put(key.to_string(), data.clone()).await;

        Ok(data)
    }

    /// Invalidate cache entries by prefix
    pub async fn invalidate_prefix(&self, prefix: &str) {
        // Note: foyer 0.12 doesn't have built-in prefix removal
        // For now, we'll just document this limitation
        // In production, we'd track keys separately or upgrade to newer foyer
        tracing::warn!("invalidate_prefix not fully implemented in foyer 0.12: {}", prefix);
    }
}

/// Cache key builders
impl CacheManager {
    /// Key for manifest
    pub fn manifest_key(namespace: &str) -> String {
        format!("manifest:{}", namespace)
    }

    /// Key for vector index
    pub fn vector_index_key(namespace: &str) -> String {
        format!("vidx:{}", namespace)
    }

    /// Key for segment data
    pub fn segment_key(namespace: &str, segment_id: &str) -> String {
        format!("seg:{}:{}", namespace, segment_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_cache_basic() {
        let temp_dir = TempDir::new().unwrap();
        let config = CacheConfig {
            memory_size: 1024 * 1024, // 1MB for test
            disk_size: 10 * 1024 * 1024, // 10MB for test
            disk_path: temp_dir.path().to_str().unwrap().to_string(),
        };

        let cache = CacheManager::new(config).await.unwrap();

        // Test put/get
        let key = "test_key".to_string();
        let value = Bytes::from("test_value");

        cache.put(key.clone(), value.clone()).await;
        let retrieved = cache.get(&key).await;

        assert_eq!(retrieved, Some(value));
    }

    #[tokio::test]
    async fn test_cache_get_or_fetch() {
        let temp_dir = TempDir::new().unwrap();
        let config = CacheConfig {
            memory_size: 1024 * 1024,
            disk_size: 10 * 1024 * 1024,
            disk_path: temp_dir.path().to_str().unwrap().to_string(),
        };

        let cache = CacheManager::new(config).await.unwrap();

        let key = "test_key";
        let expected_value = Bytes::from("fetched_value");

        // First call should fetch
        let value = cache
            .get_or_fetch(key, || async { Ok(expected_value.clone()) })
            .await
            .unwrap();

        assert_eq!(value, expected_value);

        // Second call should hit cache
        let value2 = cache
            .get_or_fetch(key, || async {
                panic!("Should not be called - cache hit expected")
            })
            .await
            .unwrap();

        assert_eq!(value2, expected_value);
    }

    #[test]
    fn test_cache_keys() {
        assert_eq!(
            CacheManager::manifest_key("my_ns"),
            "manifest:my_ns"
        );
        assert_eq!(
            CacheManager::vector_index_key("my_ns"),
            "vidx:my_ns"
        );
        assert_eq!(
            CacheManager::segment_key("my_ns", "seg_001"),
            "seg:my_ns:seg_001"
        );
    }
}
