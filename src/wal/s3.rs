//! S3-backed Write-Ahead Log
//!
//! This module implements a WAL that stores entries directly in S3/object storage,
//! enabling distributed indexer nodes to write WAL entries without coordination.
//!
//! ## Key Design Points:
//! 1. Each WAL entry is a separate object in S3
//! 2. File naming: `{namespace}/wal/{timestamp}_{node_id}.log`
//! 3. No file locking needed - S3 PUT is atomic
//! 4. Multiple indexers can write to different namespaces concurrently

use bytes::Bytes;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::storage::StorageBackend;
use crate::wal::WalOperation;
use crate::{Error, Result};

/// S3-backed WAL manager
///
/// Unlike the local WAL which uses a single file, S3 WAL uses
/// one object per operation for atomic writes.
pub struct S3WalManager {
    /// Namespace this WAL belongs to
    namespace: String,

    /// Unique node identifier (e.g., "indexer-1", "indexer-2")
    node_id: String,

    /// Storage backend (S3 or compatible)
    storage: Arc<dyn StorageBackend>,

    /// Sequence number for this node (monotonic)
    sequence: AtomicU64,
}

impl S3WalManager {
    /// Create a new S3 WAL manager
    ///
    /// # Arguments
    /// * `namespace` - Namespace name
    /// * `node_id` - Unique identifier for this node
    /// * `storage` - Storage backend
    pub fn new(namespace: String, node_id: String, storage: Arc<dyn StorageBackend>) -> Self {
        Self {
            namespace,
            node_id,
            storage,
            sequence: AtomicU64::new(0),
        }
    }

    /// Append an operation to the WAL
    ///
    /// This creates a new object in S3 with the serialized operation.
    /// The object key includes timestamp and node_id to avoid conflicts.
    ///
    /// # Returns
    /// Sequence number of the appended entry
    pub async fn append(&self, operation: WalOperation) -> Result<u64> {
        // 1. Serialize operation using MessagePack
        let mut buf = Vec::new();
        rmp_serde::encode::write(&mut buf, &operation)
            .map_err(|e| Error::internal(format!("Failed to serialize WAL operation: {}", e)))?;

        // 2. Add CRC32 checksum (4 bytes at the end)
        let crc = crc32fast::hash(&buf);
        buf.extend_from_slice(&crc.to_le_bytes());

        // 3. Generate unique key
        let seq = self.sequence.fetch_add(1, Ordering::SeqCst);
        let timestamp = chrono::Utc::now().timestamp_millis();
        let key = format!(
            "{}/wal/{:020}_{}_seq{:06}.log",
            self.namespace, timestamp, self.node_id, seq
        );

        // 4. Write to S3 (atomic operation)
        tracing::debug!(
            "Writing WAL entry to {} ({} bytes)",
            key,
            buf.len()
        );

        self.storage.put(&key, Bytes::from(buf)).await?;

        Ok(seq)
    }

    /// Synchronize WAL to storage
    ///
    /// For S3, this is a no-op since PUT operations are already durable.
    /// Kept for API compatibility with LocalWalManager.
    pub async fn sync(&self) -> Result<()> {
        // S3 PUT is immediately durable, no sync needed
        Ok(())
    }

    /// List all WAL files for this namespace
    ///
    /// Returns sorted list of WAL object keys.
    pub async fn list_wal_files(&self) -> Result<Vec<String>> {
        let prefix = format!("{}/wal/", self.namespace);

        // Use storage backend's list operation
        let files = self.storage.list(&prefix).await?;

        // Sort by timestamp (embedded in filename)
        let mut sorted_files = files;
        sorted_files.sort();

        tracing::debug!("Found {} WAL files for namespace '{}'", sorted_files.len(), self.namespace);

        Ok(sorted_files)
    }

    /// Replay all WAL entries for this namespace
    ///
    /// Reads all WAL files, verifies checksums, and returns operations.
    /// Operations are returned in timestamp order.
    pub async fn replay(&self) -> Result<Vec<WalOperation>> {
        let files = self.list_wal_files().await?;

        if files.is_empty() {
            tracing::info!("No WAL files to replay for namespace '{}'", self.namespace);
            return Ok(Vec::new());
        }

        tracing::info!("Replaying {} WAL files for namespace '{}'", files.len(), self.namespace);

        let mut operations = Vec::new();

        for file_key in files {
            match self.read_wal_entry(&file_key).await {
                Ok(op) => operations.push(op),
                Err(e) => {
                    tracing::error!("Failed to read WAL file {}: {}. Skipping.", file_key, e);
                    // Continue with other files - partial recovery is better than none
                }
            }
        }

        tracing::info!("Successfully replayed {} operations", operations.len());

        Ok(operations)
    }

    /// Read and parse a single WAL entry
    async fn read_wal_entry(&self, key: &str) -> Result<WalOperation> {
        let data = self.storage.get(key).await?;

        // Verify minimum size (at least 4 bytes for CRC)
        if data.len() < 4 {
            return Err(Error::internal(format!(
                "WAL file {} too short ({}  bytes)",
                key,
                data.len()
            )));
        }

        // Split data and checksum
        let (msg_data, crc_bytes) = data.split_at(data.len() - 4);
        let stored_crc = u32::from_le_bytes(
            crc_bytes
                .try_into()
                .map_err(|_| Error::internal("Invalid CRC bytes"))?,
        );
        let computed_crc = crc32fast::hash(msg_data);

        // Verify checksum
        if stored_crc != computed_crc {
            return Err(Error::internal(format!(
                "WAL file {} corrupted (CRC mismatch: expected {}, got {})",
                key, stored_crc, computed_crc
            )));
        }

        // Deserialize operation
        let operation: WalOperation = rmp_serde::from_slice(msg_data).map_err(|e| {
            Error::internal(format!("Failed to deserialize WAL operation from {}: {}", key, e))
        })?;

        Ok(operation)
    }

    /// Truncate WAL (delete all committed entries)
    ///
    /// This is called after successfully updating the manifest,
    /// meaning all WAL entries have been committed to durable storage.
    pub async fn truncate(&self) -> Result<()> {
        let files = self.list_wal_files().await?;

        if files.is_empty() {
            return Ok(());
        }

        tracing::info!("Truncating {} WAL files for namespace '{}'", files.len(), self.namespace);

        // Delete all WAL files for this namespace
        for file_key in files {
            if let Err(e) = self.storage.delete(&file_key).await {
                tracing::warn!("Failed to delete WAL file {}: {}", file_key, e);
                // Continue deleting other files
            }
        }

        Ok(())
    }

    /// Get the namespace this WAL manages
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Get the node ID
    pub fn node_id(&self) -> &str {
        &self.node_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::local::LocalStorage;
    use crate::types::Document;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_s3_wal_basic() {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());
        let wal = S3WalManager::new("test_ns".to_string(), "node1".to_string(), storage);

        // Append operation
        let op = WalOperation::Upsert {
            documents: vec![Document {
                id: 1,
                vector: Some(vec![1.0, 2.0, 3.0]),
                attributes: Default::default(),
            }],
        };

        let seq = wal.append(op.clone()).await.unwrap();
        assert_eq!(seq, 0);

        // Replay
        let operations = wal.replay().await.unwrap();
        assert_eq!(operations.len(), 1);

        match &operations[0] {
            WalOperation::Upsert { documents } => {
                assert_eq!(documents.len(), 1);
                assert_eq!(documents[0].id, 1);
            }
        }
    }

    #[tokio::test]
    async fn test_s3_wal_multiple_entries() {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());
        let wal = S3WalManager::new("test_ns".to_string(), "node1".to_string(), storage);

        // Append multiple operations
        for i in 0..5 {
            let op = WalOperation::Upsert {
                documents: vec![Document {
                    id: i,
                    vector: Some(vec![i as f32]),
                    attributes: Default::default(),
                }],
            };
            wal.append(op).await.unwrap();
        }

        // Replay should return all operations in order
        let operations = wal.replay().await.unwrap();
        assert_eq!(operations.len(), 5);

        for (i, op) in operations.iter().enumerate() {
            match op {
                WalOperation::Upsert { documents } => {
                    assert_eq!(documents[0].id, i as u64);
                }
            }
        }
    }

    #[tokio::test]
    async fn test_s3_wal_truncate() {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());
        let wal = S3WalManager::new("test_ns".to_string(), "node1".to_string(), storage);

        // Append operations
        let op = WalOperation::Upsert {
            documents: vec![Document {
                id: 1,
                vector: Some(vec![1.0]),
                attributes: Default::default(),
            }],
        };
        wal.append(op).await.unwrap();

        // Verify exists
        let operations = wal.replay().await.unwrap();
        assert_eq!(operations.len(), 1);

        // Truncate
        wal.truncate().await.unwrap();

        // Verify empty
        let operations = wal.replay().await.unwrap();
        assert_eq!(operations.len(), 0);
    }

    #[tokio::test]
    async fn test_s3_wal_multi_node() {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());

        // Two nodes writing to same namespace
        let wal1 = S3WalManager::new("test_ns".to_string(), "node1".to_string(), storage.clone());
        let wal2 = S3WalManager::new("test_ns".to_string(), "node2".to_string(), storage.clone());

        // Node 1 writes
        let op1 = WalOperation::Upsert {
            documents: vec![Document {
                id: 1,
                vector: Some(vec![1.0]),
                attributes: Default::default(),
            }],
        };
        wal1.append(op1).await.unwrap();

        // Node 2 writes
        let op2 = WalOperation::Upsert {
            documents: vec![Document {
                id: 2,
                vector: Some(vec![2.0]),
                attributes: Default::default(),
            }],
        };
        wal2.append(op2).await.unwrap();

        // Both nodes can replay all entries for the namespace
        let operations1 = wal1.replay().await.unwrap();
        let operations2 = wal2.replay().await.unwrap();

        assert_eq!(operations1.len(), 2);
        assert_eq!(operations2.len(), 2);
    }
}
