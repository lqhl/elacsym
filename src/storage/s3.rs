//! S3 storage backend

use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use bytes::Bytes;

use crate::{Error, Result};

use super::StorageBackend;

/// S3 storage backend
pub struct S3Storage {
    client: Client,
    bucket: String,
}

impl S3Storage {
    pub async fn new(
        bucket: String,
        region: String,
        endpoint: Option<String>,
    ) -> Result<Self> {
        let config = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(region))
            .load()
            .await;

        let mut s3_config_builder = aws_sdk_s3::config::Builder::from(&config);

        if let Some(endpoint_url) = endpoint {
            s3_config_builder = s3_config_builder
                .endpoint_url(endpoint_url)
                .force_path_style(true);
        }

        let s3_config = s3_config_builder.build();
        let client = Client::from_conf(s3_config);

        Ok(Self { client, bucket })
    }
}

#[async_trait]
impl StorageBackend for S3Storage {
    async fn get(&self, key: &str) -> Result<Bytes> {
        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| Error::storage(format!("S3 get failed: {}", e)))?;

        let data = response
            .body
            .collect()
            .await
            .map_err(|e| Error::storage(format!("S3 body read failed: {}", e)))?;

        Ok(data.into_bytes())
    }

    async fn put(&self, key: &str, data: Bytes) -> Result<()> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(data))
            .send()
            .await
            .map_err(|e| Error::storage(format!("S3 put failed: {}", e)))?;

        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| Error::storage(format!("S3 delete failed: {}", e)))?;

        Ok(())
    }

    async fn exists(&self, key: &str) -> Result<bool> {
        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let response = self
            .client
            .list_objects_v2()
            .bucket(&self.bucket)
            .prefix(prefix)
            .send()
            .await
            .map_err(|e| Error::storage(format!("S3 list failed: {}", e)))?;

        let keys = response
            .contents()
            .iter()
            .filter_map(|obj| obj.key().map(|k| k.to_string()))
            .collect();

        Ok(keys)
    }

    async fn get_range(&self, key: &str, start: u64, end: u64) -> Result<Bytes> {
        let range = format!("bytes={}-{}", start, end - 1);

        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .range(range)
            .send()
            .await
            .map_err(|e| Error::storage(format!("S3 range get failed: {}", e)))?;

        let data = response
            .body
            .collect()
            .await
            .map_err(|e| Error::storage(format!("S3 body read failed: {}", e)))?;

        Ok(data.into_bytes())
    }
}
