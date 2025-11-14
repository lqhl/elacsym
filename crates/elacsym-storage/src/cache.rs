//! LRU cache for slabs

use crate::SlabStorage;
use async_trait::async_trait;
use elacsym_core::{Result, Slab, SlabId};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info};

/// Cache configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    /// Maximum cache size in bytes
    pub max_size_bytes: usize,

    /// Maximum number of slabs to cache
    pub max_slabs: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_size_bytes: 1024 * 1024 * 1024, // 1GB
            max_slabs: 1000,
        }
    }
}

/// Cache entry
struct CacheEntry {
    slab: Arc<Slab>,
    size_bytes: usize,
    last_access: std::time::Instant,
}

/// LRU cache for slabs
pub struct SlabCache<S: SlabStorage> {
    config: CacheConfig,
    storage: Arc<S>,
    cache: Arc<RwLock<HashMap<SlabId, CacheEntry>>>,
    total_size: Arc<RwLock<usize>>,
}

impl<S: SlabStorage> SlabCache<S> {
    pub fn new(config: CacheConfig, storage: Arc<S>) -> Self {
        Self {
            config,
            storage,
            cache: Arc::new(RwLock::new(HashMap::new())),
            total_size: Arc::new(RwLock::new(0)),
        }
    }

    /// Evict least recently used entries if cache is full
    fn evict_if_needed(&self, needed_size: usize) {
        let mut cache = self.cache.write();
        let mut total_size = self.total_size.write();

        while *total_size + needed_size > self.config.max_size_bytes
            || cache.len() >= self.config.max_slabs
        {
            if cache.is_empty() {
                break;
            }

            // Find LRU entry
            let lru_id = cache
                .iter()
                .min_by_key(|(_, entry)| entry.last_access)
                .map(|(id, _)| id.clone());

            if let Some(id) = lru_id {
                if let Some(entry) = cache.remove(&id) {
                    *total_size -= entry.size_bytes;
                    debug!("Evicted slab {} from cache", id);
                }
            }
        }
    }

    /// Get cache statistics
    pub fn stats(&self) -> CacheStats {
        let cache = self.cache.read();
        let total_size = *self.total_size.read();

        CacheStats {
            entry_count: cache.len(),
            total_size_bytes: total_size,
            max_size_bytes: self.config.max_size_bytes,
            hit_rate: 0.0, // TODO: Track hits/misses
        }
    }

    /// Clear cache
    pub fn clear(&self) {
        let mut cache = self.cache.write();
        let mut total_size = self.total_size.write();

        cache.clear();
        *total_size = 0;

        info!("Cache cleared");
    }
}

#[async_trait]
impl<S: SlabStorage> SlabStorage for SlabCache<S> {
    async fn put_slab(&self, slab: &Slab) -> Result<()> {
        // Store to underlying storage
        self.storage.put_slab(slab).await?;

        // Add to cache
        let size_bytes = slab.size_bytes();
        self.evict_if_needed(size_bytes);

        let entry = CacheEntry {
            slab: Arc::new(slab.clone()),
            size_bytes,
            last_access: std::time::Instant::now(),
        };

        let mut cache = self.cache.write();
        let mut total_size = self.total_size.write();

        cache.insert(slab.metadata.id.clone(), entry);
        *total_size += size_bytes;

        debug!("Cached slab {}", slab.metadata.id);

        Ok(())
    }

    async fn get_slab(&self, id: &SlabId) -> Result<Option<Slab>> {
        // Check cache first
        {
            let mut cache = self.cache.write();
            if let Some(entry) = cache.get_mut(id) {
                entry.last_access = std::time::Instant::now();
                debug!("Cache hit for slab {}", id);
                return Ok(Some((*entry.slab).clone()));
            }
        }

        debug!("Cache miss for slab {}", id);

        // Load from storage
        let slab_opt = self.storage.get_slab(id).await?;

        if let Some(slab) = &slab_opt {
            // Add to cache
            let size_bytes = slab.size_bytes();
            self.evict_if_needed(size_bytes);

            let entry = CacheEntry {
                slab: Arc::new(slab.clone()),
                size_bytes,
                last_access: std::time::Instant::now(),
            };

            let mut cache = self.cache.write();
            let mut total_size = self.total_size.write();

            cache.insert(id.clone(), entry);
            *total_size += size_bytes;
        }

        Ok(slab_opt)
    }

    async fn delete_slab(&self, id: &SlabId) -> Result<()> {
        // Remove from cache
        {
            let mut cache = self.cache.write();
            let mut total_size = self.total_size.write();

            if let Some(entry) = cache.remove(id) {
                *total_size -= entry.size_bytes;
            }
        }

        // Delete from storage
        self.storage.delete_slab(id).await
    }

    async fn list_slabs(&self, namespace: &str) -> Result<Vec<SlabId>> {
        self.storage.list_slabs(namespace).await
    }

    async fn exists(&self, id: &SlabId) -> Result<bool> {
        // Check cache first
        {
            let cache = self.cache.read();
            if cache.contains_key(id) {
                return Ok(true);
            }
        }

        // Check storage
        self.storage.exists(id).await
    }
}

/// Cache statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStats {
    pub entry_count: usize,
    pub total_size_bytes: usize,
    pub max_size_bytes: usize,
    pub hit_rate: f64,
}
