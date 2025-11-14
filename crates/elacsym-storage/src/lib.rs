//! elacsym-storage: Storage layer for slabs and metadata

pub mod blob;
pub mod cache;

pub use blob::{BlobStorage, BlobStorageConfig};
pub use cache::{SlabCache, CacheConfig};

use elacsym_core::{Result, Slab, SlabId};
use async_trait::async_trait;

/// Storage interface for slabs
#[async_trait]
pub trait SlabStorage: Send + Sync {
    /// Store a slab
    async fn put_slab(&self, slab: &Slab) -> Result<()>;

    /// Retrieve a slab
    async fn get_slab(&self, id: &SlabId) -> Result<Option<Slab>>;

    /// Delete a slab
    async fn delete_slab(&self, id: &SlabId) -> Result<()>;

    /// List all slabs in a namespace
    async fn list_slabs(&self, namespace: &str) -> Result<Vec<SlabId>>;

    /// Check if slab exists
    async fn exists(&self, id: &SlabId) -> Result<bool>;
}
