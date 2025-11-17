//! Write-Ahead Log (WAL) implementation using S3
//!
//! This module provides a distributed, S3-based WAL that replaces the in-memory
//! Freshness Layer. It supports batch writes with acceptable latency and enables
//! coordination-free distributed writes.

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use elacsym_core::{Vector, VectorId};
use object_store::{path::Path, ObjectStore};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

/// A batch of vectors written to the WAL
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WALBatch {
    /// Unique batch ID
    pub batch_id: String,
    /// Timestamp when batch was written
    pub timestamp: DateTime<Utc>,
    /// Namespace for these vectors
    pub namespace: String,
    /// Vectors in this batch
    pub vectors: Vec<Vector>,
    /// Whether this batch has been indexed
    pub indexed: bool,
}

impl WALBatch {
    pub fn new(namespace: String, vectors: Vec<Vector>) -> Self {
        Self {
            batch_id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            namespace,
            vectors,
            indexed: false,
        }
    }
}

/// Delete entry in the WAL
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WALDelete {
    /// Unique delete ID
    pub delete_id: String,
    /// Timestamp when delete was written
    pub timestamp: DateTime<Utc>,
    /// Namespace for this delete
    pub namespace: String,
    /// Vector IDs to delete
    pub vector_ids: Vec<VectorId>,
}

impl WALDelete {
    pub fn new(namespace: String, vector_ids: Vec<VectorId>) -> Self {
        Self {
            delete_id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            namespace,
            vector_ids,
        }
    }
}

/// WAL entry type
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WALEntry {
    Batch(WALBatch),
    Delete(WALDelete),
}

/// S3-based Write-Ahead Log
pub struct S3WAL {
    store: Arc<dyn ObjectStore>,
    base_path: String,
}

impl S3WAL {
    /// Create a new S3 WAL
    pub fn new(store: Arc<dyn ObjectStore>, base_path: String) -> Self {
        Self { store, base_path }
    }

    /// Get the path for a WAL batch entry
    fn batch_path(&self, namespace: &str, timestamp: &DateTime<Utc>, batch_id: &str) -> Path {
        let ts_str = timestamp.format("%Y%m%d-%H%M%S-%f");
        Path::from(format!(
            "{}/wal/{}/batches/{}-{}.json",
            self.base_path, namespace, ts_str, batch_id
        ))
    }

    /// Get the path for a WAL delete entry
    fn delete_path(&self, namespace: &str, timestamp: &DateTime<Utc>, delete_id: &str) -> Path {
        let ts_str = timestamp.format("%Y%m%d-%H%M%S-%f");
        Path::from(format!(
            "{}/wal/{}/deletes/{}-{}.json",
            self.base_path, namespace, ts_str, delete_id
        ))
    }

    /// Write a batch of vectors to the WAL
    pub async fn write_batch(&self, batch: WALBatch) -> Result<()> {
        let path = self.batch_path(&batch.namespace, &batch.timestamp, &batch.batch_id);
        let data = serde_json::to_vec_pretty(&batch)?;
        self.store.put(&path, data.into()).await?;
        Ok(())
    }

    /// Write a delete entry to the WAL
    pub async fn write_delete(&self, delete: WALDelete) -> Result<()> {
        let path = self.delete_path(&delete.namespace, &delete.timestamp, &delete.delete_id);
        let data = serde_json::to_vec_pretty(&delete)?;
        self.store.put(&path, data.into()).await?;
        Ok(())
    }

    /// List all WAL batches for a namespace
    pub async fn list_batches(&self, namespace: &str) -> Result<Vec<WALBatch>> {
        let prefix = Path::from(format!("{}/wal/{}/batches/", self.base_path, namespace));
        let list_result = self.store.list(Some(&prefix)).await?;

        let mut batches = Vec::new();
        for meta in list_result {
            let data = self.store.get(&meta.location).await?;
            let bytes = data.bytes().await?;
            let batch: WALBatch = serde_json::from_slice(&bytes)?;
            batches.push(batch);
        }

        // Sort by timestamp (oldest first)
        batches.sort_by_key(|b| b.timestamp);
        Ok(batches)
    }

    /// List all WAL deletes for a namespace
    pub async fn list_deletes(&self, namespace: &str) -> Result<Vec<WALDelete>> {
        let prefix = Path::from(format!("{}/wal/{}/deletes/", self.base_path, namespace));
        let list_result = self.store.list(Some(&prefix)).await?;

        let mut deletes = Vec::new();
        for meta in list_result {
            let data = self.store.get(&meta.location).await?;
            let bytes = data.bytes().await?;
            let delete: WALDelete = serde_json::from_slice(&bytes)?;
            deletes.push(delete);
        }

        // Sort by timestamp (oldest first)
        deletes.sort_by_key(|d| d.timestamp);
        Ok(deletes)
    }

    /// Get all unindexed vectors from WAL for a namespace
    pub async fn get_unindexed_vectors(&self, namespace: &str) -> Result<Vec<Vector>> {
        let batches = self.list_batches(namespace).await?;
        let mut vectors = Vec::new();

        for batch in batches {
            if !batch.indexed {
                vectors.extend(batch.vectors);
            }
        }

        Ok(vectors)
    }

    /// Get all deleted vector IDs from WAL for a namespace
    pub async fn get_deleted_ids(&self, namespace: &str) -> Result<Vec<VectorId>> {
        let deletes = self.list_deletes(namespace).await?;
        let mut deleted_ids = Vec::new();

        for delete in deletes {
            deleted_ids.extend(delete.vector_ids);
        }

        Ok(deleted_ids)
    }

    /// Mark a batch as indexed
    pub async fn mark_batch_indexed(&self, namespace: &str, batch_id: &str) -> Result<()> {
        let batches = self.list_batches(namespace).await?;

        for mut batch in batches {
            if batch.batch_id == batch_id {
                batch.indexed = true;
                let path = self.batch_path(&batch.namespace, &batch.timestamp, &batch.batch_id);
                let data = serde_json::to_vec_pretty(&batch)?;
                self.store.put(&path, data.into()).await?;
                break;
            }
        }

        Ok(())
    }

    /// Clean up indexed batches older than a certain time
    pub async fn cleanup_indexed_batches(
        &self,
        namespace: &str,
        before: DateTime<Utc>,
    ) -> Result<usize> {
        let batches = self.list_batches(namespace).await?;
        let mut cleaned = 0;

        for batch in batches {
            if batch.indexed && batch.timestamp < before {
                let path = self.batch_path(&batch.namespace, &batch.timestamp, &batch.batch_id);
                self.store.delete(&path).await?;
                cleaned += 1;
            }
        }

        Ok(cleaned)
    }

    /// Clean up old delete entries
    pub async fn cleanup_old_deletes(
        &self,
        namespace: &str,
        before: DateTime<Utc>,
    ) -> Result<usize> {
        let deletes = self.list_deletes(namespace).await?;
        let mut cleaned = 0;

        for delete in deletes {
            if delete.timestamp < before {
                let path = self.delete_path(&delete.namespace, &delete.timestamp, &delete.delete_id);
                self.store.delete(&path).await?;
                cleaned += 1;
            }
        }

        Ok(cleaned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use elacsym_core::Metadata;
    use object_store::memory::InMemory;

    fn create_test_vector(id: &str, namespace: &str) -> Vector {
        Vector::new(
            VectorId::from(id.to_string()),
            vec![1.0, 2.0, 3.0],
            Metadata::new(),
            namespace,
        )
    }

    #[tokio::test]
    async fn test_wal_batch_write_and_read() {
        let store = Arc::new(InMemory::new());
        let wal = S3WAL::new(store, "test".to_string());

        let vectors = vec![
            create_test_vector("vec1", "test_ns"),
            create_test_vector("vec2", "test_ns"),
        ];

        let batch = WALBatch::new("test_ns".to_string(), vectors);
        let batch_id = batch.batch_id.clone();

        wal.write_batch(batch).await.unwrap();

        let batches = wal.list_batches("test_ns").await.unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].batch_id, batch_id);
        assert_eq!(batches[0].vectors.len(), 2);
        assert!(!batches[0].indexed);
    }

    #[tokio::test]
    async fn test_wal_delete_write_and_read() {
        let store = Arc::new(InMemory::new());
        let wal = S3WAL::new(store, "test".to_string());

        let delete = WALDelete::new(
            "test_ns".to_string(),
            vec![VectorId::from("vec1"), VectorId::from("vec2")],
        );

        wal.write_delete(delete).await.unwrap();

        let deletes = wal.list_deletes("test_ns").await.unwrap();
        assert_eq!(deletes.len(), 1);
        assert_eq!(deletes[0].vector_ids.len(), 2);
    }

    #[tokio::test]
    async fn test_mark_batch_indexed() {
        let store = Arc::new(InMemory::new());
        let wal = S3WAL::new(store, "test".to_string());

        let vectors = vec![create_test_vector("vec1", "test_ns")];
        let batch = WALBatch::new("test_ns".to_string(), vectors);
        let batch_id = batch.batch_id.clone();

        wal.write_batch(batch).await.unwrap();

        // Mark as indexed
        wal.mark_batch_indexed("test_ns", &batch_id).await.unwrap();

        let batches = wal.list_batches("test_ns").await.unwrap();
        assert_eq!(batches.len(), 1);
        assert!(batches[0].indexed);
    }

    #[tokio::test]
    async fn test_get_unindexed_vectors() {
        let store = Arc::new(InMemory::new());
        let wal = S3WAL::new(store, "test".to_string());

        // Write two batches
        let batch1 = WALBatch::new(
            "test_ns".to_string(),
            vec![create_test_vector("vec1", "test_ns")],
        );
        let batch2 = WALBatch::new(
            "test_ns".to_string(),
            vec![create_test_vector("vec2", "test_ns")],
        );

        wal.write_batch(batch1.clone()).await.unwrap();
        wal.write_batch(batch2).await.unwrap();

        // Mark first batch as indexed
        wal.mark_batch_indexed("test_ns", &batch1.batch_id).await.unwrap();

        // Should only get vectors from unindexed batch
        let vectors = wal.get_unindexed_vectors("test_ns").await.unwrap();
        assert_eq!(vectors.len(), 1);
        assert_eq!(vectors[0].id.as_str(), "vec2");
    }
}
