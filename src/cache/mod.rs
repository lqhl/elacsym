//! Cache layer using foyer

use bytes::Bytes;

use crate::Result;

/// Cache configuration
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

/// Cache manager
///
/// NOTE: Foyer integration is temporarily disabled due to API changes
/// We'll implement this in a later phase
pub struct CacheManager {
    _config: CacheConfig,
}

impl CacheManager {
    pub async fn new(config: CacheConfig) -> Result<Self> {
        Ok(Self { _config: config })
    }

    pub async fn get(&self, _key: &str) -> Option<Bytes> {
        None
    }

    pub async fn put(&self, _key: String, _value: Bytes) {
        // TODO: Implement with foyer
    }

    pub async fn remove(&self, _key: &str) {
        // TODO: Implement with foyer
    }
}
