#![allow(dead_code)]

//! Abstractions around S3 and compatible object storage providers.
//!
//! The production system will lean on serverless primitives, so providing a
//! thin, async-friendly façade here keeps the higher level crates agnostic to
//! whether data is fetched via AWS SDK, Opendal, or another adaptor.

use async_trait::async_trait;
use bytes::Bytes;
use common::{Error, Result};
use tracing::instrument;

/// Minimal capability set required by the rest of the workspace.
#[async_trait]
pub trait ObjectStore: Send + Sync {
    /// Fetches the entire object located at `key`.
    async fn get(&self, key: &str) -> Result<Bytes>;

    /// Writes a complete object, overwriting any previous value.
    async fn put(&self, key: &str, data: Bytes) -> Result<()>;

    /// Conditionally swaps the object if the supplied `if_match` etag is still valid.
    async fn put_if_match(&self, key: &str, data: Bytes, if_match: &str) -> Result<()>;
}

/// Placeholder implementation illustrating how an S3-backed store might look.
pub struct S3Store {
    bucket: String,
}

impl S3Store {
    /// Create a new store that targets the provided bucket.
    pub fn new(bucket: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
        }
    }
}

#[async_trait]
impl ObjectStore for S3Store {
    #[instrument(skip(self))]
    async fn get(&self, key: &str) -> Result<Bytes> {
        Err(Error::Message(format!(
            "not yet implemented: download {key} from {}",
            self.bucket
        )))
    }

    #[instrument(skip(self, _data))]
    async fn put(&self, key: &str, _data: Bytes) -> Result<()> {
        Err(Error::Message(format!(
            "not yet implemented: upload {key} to {}",
            self.bucket
        )))
    }

    #[instrument(skip(self, _data))]
    async fn put_if_match(&self, key: &str, _data: Bytes, if_match: &str) -> Result<()> {
        Err(Error::Message(format!(
            "not yet implemented: upload {key} to {} guarded by etag {if_match}",
            self.bucket
        )))
    }
}

/// Helper that wraps an operation with additional context.
pub async fn with_storage_context<F, Fut, T>(key: &str, f: F) -> Result<T>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    match f().await {
        Ok(value) => Ok(value),
        Err(err) => Err(Error::Message(format!("{err} (while operating on {key})"))),
    }
}
