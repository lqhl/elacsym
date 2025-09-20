//! Storage layer abstractions for WAL, parts, and object store clients.

use anyhow::Result;

/// Placeholder trait for object store interaction.
#[async_trait::async_trait]
pub trait ObjectStore: Send + Sync {
    async fn put(&self, key: &str, payload: &[u8]) -> Result<()>;
}

/// Placeholder no-op object store used for scaffolding.
pub struct NoopStore;

#[async_trait::async_trait]
impl ObjectStore for NoopStore {
    async fn put(&self, _key: &str, _payload: &[u8]) -> Result<()> {
        Ok(())
    }
}
