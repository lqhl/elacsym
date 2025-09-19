#![allow(dead_code)]

//! Abstractions around S3 and compatible object storage providers.
//!
//! The production system will lean on serverless primitives, so providing a
//! thin, async-friendly façade here keeps the higher level crates agnostic to
//! whether data is fetched via AWS SDK, Opendal, or another adaptor.

use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_sdk_s3::{primitives::ByteStream, Client};
use bytes::Bytes;
use common::{Error, Result};
use std::sync::Arc;
use tracing::instrument;

/// Minimal capability set required by the rest of the workspace.
#[async_trait]
pub trait ObjectStore: Send + Sync {
    /// Fetches the entire object located at `key`.
    async fn get(&self, key: &str) -> Result<ObjectBytes>;

    /// Writes a complete object, overwriting any previous value.
    async fn put(&self, key: &str, data: Bytes) -> Result<String>;

    /// Conditionally swaps the object if the supplied `if_match` etag is still valid.
    async fn put_if_match(&self, key: &str, data: Bytes, if_match: &str) -> Result<String>;
}

/// Response payload returned from [`ObjectStore::get`].
#[derive(Debug, Clone)]
pub struct ObjectBytes {
    /// Raw object contents.
    pub data: Bytes,
    /// Entity tag (ETag) returned by the backing store, if available.
    pub etag: Option<String>,
}

impl ObjectBytes {
    /// Convenience helper for constructing a response with known metadata.
    pub fn new(data: Bytes, etag: Option<String>) -> Self {
        Self { data, etag }
    }
}

/// Placeholder implementation illustrating how an S3-backed store might look.
pub struct S3Store {
    bucket: String,
    client: Arc<Client>,
}

impl S3Store {
    /// Create a new store that targets the provided bucket.
    pub fn new(client: Client, bucket: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
            client: Arc::new(client),
        }
    }

    /// Constructs a store using default AWS configuration from the environment.
    pub async fn from_env(bucket: impl Into<String>) -> Result<Self> {
        let config = aws_config::load_defaults(BehaviorVersion::latest()).await;
        let client = Client::new(&config);
        Ok(Self::new(client, bucket))
    }
}

#[async_trait]
impl ObjectStore for S3Store {
    #[instrument(skip(self))]
    async fn get(&self, key: &str) -> Result<ObjectBytes> {
        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|err| Error::Context(err.into()))?;

        let etag = response.e_tag().map(|etag| etag.to_string());

        let bytes = response
            .body
            .collect()
            .await
            .map_err(|err| Error::Context(err.into()))?
            .into_bytes();

        Ok(ObjectBytes::new(bytes, etag))
    }

    #[instrument(skip(self, data))]
    async fn put(&self, key: &str, data: Bytes) -> Result<String> {
        let response = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(data.to_vec()))
            .send()
            .await
            .map_err(|err| Error::Context(err.into()))?;

        response
            .e_tag()
            .map(|etag| etag.to_string())
            .ok_or_else(|| Error::from("S3 did not return an ETag for put"))
    }

    #[instrument(skip(self, data))]
    async fn put_if_match(&self, key: &str, data: Bytes, if_match: &str) -> Result<String> {
        let response = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .if_match(if_match)
            .body(ByteStream::from(data.to_vec()))
            .send()
            .await
            .map_err(|err| Error::Context(err.into()))?;

        response
            .e_tag()
            .map(|etag| etag.to_string())
            .ok_or_else(|| Error::from("S3 did not return an ETag for conditional put"))
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
