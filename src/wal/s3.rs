//! S3-backed Write-Ahead Log
//!
//! This module implements a WAL that stores entries directly in S3/object storage,
//! enabling distributed indexer nodes to write WAL entries without coordination.
//!
//! ## Key Design Points:
//! 1. Each WAL entry is a separate object in S3
//! 2. File naming: `[{prefix}/]wal/{namespace}/{node_id}/{timestamp}_seq{n}.log`
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

    /// Optional key prefix for multi-tenant buckets
    prefix: Option<String>,
}

impl S3WalManager {
    /// Create a new S3 WAL manager
    ///
    /// # Arguments
    /// * `namespace` - Namespace name
    /// * `node_id` - Unique identifier for this node
    /// * `storage` - Storage backend
    pub fn new(
        namespace: String,
        node_id: String,
        storage: Arc<dyn StorageBackend>,
        prefix: Option<String>,
    ) -> Self {
        Self {
            namespace,
            node_id,
            storage,
            sequence: AtomicU64::new(0),
            prefix,
        }
    }

    fn namespace_prefix(&self) -> String {
        match self
            .prefix
            .as_ref()
            .map(|p| p.trim_matches('/'))
            .filter(|p| !p.is_empty())
        {
            Some(prefix) => format!("{}/wal/{}/", prefix, self.namespace),
            None => format!("wal/{}/", self.namespace),
        }
    }

    fn wal_directory(&self) -> String {
        format!("{}{}{}", self.namespace_prefix(), self.node_id, "/")
    }

    fn wal_key(&self, seq: u64, timestamp: i64) -> String {
        let base_dir = self.wal_directory();
        format!("{}{:020}_seq{:06}.log", base_dir, timestamp, seq)
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
        let key = self.wal_key(seq, timestamp);

        // 4. Write to S3 (atomic operation)
        tracing::debug!(key = %key, size = buf.len(), "Writing WAL entry to object storage");

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
        let prefix = self.namespace_prefix();

        // Use storage backend's list operation
        let files = self.storage.list(&prefix).await?;

        // Sort by timestamp (embedded in filename)
        let mut sorted_files = files;
        sorted_files.sort();

        tracing::debug!(
            "Found {} WAL files for namespace '{}'",
            sorted_files.len(),
            self.namespace
        );

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

        tracing::info!(
            "Replaying {} WAL files for namespace '{}'",
            files.len(),
            self.namespace
        );

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
            Error::internal(format!(
                "Failed to deserialize WAL operation from {}: {}",
                key, e
            ))
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

        tracing::info!(
            "Truncating {} WAL files for namespace '{}'",
            files.len(),
            self.namespace
        );

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
    use crate::Error;
    use async_trait::async_trait;
    use bytes::BytesMut;
    use std::collections::{HashMap, HashSet};
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    #[derive(Default)]
    struct MockStorage {
        files: Mutex<HashMap<String, Bytes>>,
        fail_delete: Mutex<HashSet<String>>,
        list_error: Mutex<Option<String>>,
    }

    impl MockStorage {
        fn new() -> Self {
            Self::default()
        }

        async fn set_delete_failure(&self, key: String) {
            self.fail_delete.lock().await.insert(key);
        }

        async fn set_list_error(&self, error: impl Into<String>) {
            *self.list_error.lock().await = Some(error.into());
        }
    }

    #[async_trait]
    impl StorageBackend for MockStorage {
        async fn get(&self, key: &str) -> Result<Bytes> {
            let files = self.files.lock().await;
            files
                .get(key)
                .cloned()
                .ok_or_else(|| Error::storage(format!("missing key: {}", key)))
        }

        async fn put(&self, key: &str, data: Bytes) -> Result<()> {
            let mut files = self.files.lock().await;
            files.insert(key.to_string(), data);
            Ok(())
        }

        async fn delete(&self, key: &str) -> Result<()> {
            let fail_guard = self.fail_delete.lock().await;
            if fail_guard.contains(key) {
                return Err(Error::storage(format!("forced delete failure for {}", key)));
            }
            drop(fail_guard);

            let mut files = self.files.lock().await;
            files.remove(key);
            Ok(())
        }

        async fn exists(&self, key: &str) -> Result<bool> {
            let files = self.files.lock().await;
            Ok(files.contains_key(key))
        }

        async fn list(&self, prefix: &str) -> Result<Vec<String>> {
            if let Some(err) = self.list_error.lock().await.clone() {
                return Err(Error::storage(err));
            }

            let files = self.files.lock().await;
            let mut keys: Vec<String> = files
                .keys()
                .filter(|k| k.starts_with(prefix))
                .cloned()
                .collect();
            keys.sort();
            Ok(keys)
        }

        async fn get_range(&self, key: &str, start: u64, end: u64) -> Result<Bytes> {
            let files = self.files.lock().await;
            let data = files
                .get(key)
                .ok_or_else(|| Error::storage(format!("missing key: {}", key)))?;
            let start = start as usize;
            let end = end as usize;
            let mut buf = BytesMut::with_capacity(end - start);
            buf.extend_from_slice(&data[start..end]);
            Ok(buf.freeze())
        }
    }

    #[tokio::test]
    async fn test_s3_wal_basic() {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());
        let wal = S3WalManager::new("test_ns".to_string(), "node1".to_string(), storage, None);

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
            _ => panic!("Expected Upsert operation"),
        }
    }

    #[tokio::test]
    async fn test_s3_wal_multiple_entries() {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());
        let wal = S3WalManager::new("test_ns".to_string(), "node1".to_string(), storage, None);

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
                _ => panic!("Expected Upsert operation"),
            }
        }
    }

    #[tokio::test]
    async fn test_s3_wal_prefix_and_truncate() {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());
        let wal = S3WalManager::new(
            "tenant_ns".to_string(),
            "node-a".to_string(),
            storage.clone(),
            Some("tenant-a".to_string()),
        );

        let op = WalOperation::Upsert {
            documents: vec![Document {
                id: 42,
                vector: None,
                attributes: Default::default(),
            }],
        };

        wal.append(op).await.unwrap();
        wal.sync().await.unwrap();

        let files = wal.list_wal_files().await.unwrap();
        assert_eq!(files.len(), 1);
        let normalized = files[0].replace('\\', "/");
        assert!(normalized.starts_with("tenant-a/wal/tenant_ns/node-a/"));

        wal.truncate().await.unwrap();

        let files_after = wal.list_wal_files().await.unwrap();
        assert!(files_after.is_empty());
    }

    #[tokio::test]
    async fn test_s3_wal_truncate() {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());
        let wal = S3WalManager::new("test_ns".to_string(), "node1".to_string(), storage, None);

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
        let wal1 = S3WalManager::new(
            "test_ns".to_string(),
            "node1".to_string(),
            storage.clone(),
            None,
        );
        let wal2 = S3WalManager::new(
            "test_ns".to_string(),
            "node2".to_string(),
            storage.clone(),
            None,
        );

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

    #[tokio::test]
    async fn test_s3_wal_rotation_partial_failure() {
        let storage = Arc::new(MockStorage::new());
        let wal = S3WalManager::new(
            "fail_ns".to_string(),
            "node1".to_string(),
            storage.clone(),
            None,
        );

        for i in 0..3 {
            let op = WalOperation::Upsert {
                documents: vec![Document {
                    id: i,
                    vector: Some(vec![i as f32]),
                    attributes: Default::default(),
                }],
            };
            wal.append(op).await.unwrap();
        }

        let keys = wal.list_wal_files().await.unwrap();
        assert_eq!(keys.len(), 3);

        storage.set_delete_failure(keys[1].clone()).await;

        wal.truncate().await.unwrap();

        let remaining = storage.list("wal/fail_ns/").await.unwrap();
        assert_eq!(remaining, vec![keys[1].clone()]);
    }

    #[tokio::test]
    async fn test_s3_wal_corruption_recovery() {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());
        let wal = S3WalManager::new(
            "corrupt_ns".to_string(),
            "node1".to_string(),
            storage.clone(),
            None,
        );

        for i in 0..2 {
            let op = WalOperation::Upsert {
                documents: vec![Document {
                    id: i,
                    vector: Some(vec![i as f32]),
                    attributes: Default::default(),
                }],
            };
            wal.append(op).await.unwrap();
        }

        let files = wal.list_wal_files().await.unwrap();
        assert_eq!(files.len(), 2);

        let corrupt_path = temp_dir.path().join(&files[0]);
        let mut data = tokio::fs::read(&corrupt_path).await.unwrap();
        let last_index = data.len() - 1;
        data[last_index] ^= 0xFF;
        tokio::fs::write(&corrupt_path, &data).await.unwrap();

        let operations = wal.replay().await.unwrap();
        assert_eq!(operations.len(), 1);
        if let WalOperation::Upsert { documents } = &operations[0] {
            assert_eq!(documents[0].id, 1);
        } else {
            panic!("expected upsert operation");
        }
    }

    #[tokio::test]
    async fn test_s3_wal_list_failure() {
        let storage = Arc::new(MockStorage::new());
        storage.set_list_error("list failed").await;
        let wal = S3WalManager::new("list_ns".to_string(), "node1".to_string(), storage, None);

        let err = wal.list_wal_files().await.unwrap_err();
        assert!(err.to_string().contains("list failed"));
    }

    #[tokio::test]
    async fn test_s3_wal_concurrent_writes() {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());
        let namespace = "concurrent_ns".to_string();
        let nodes = 4;
        let writes_per_node = 5;

        let mut handles = Vec::new();
        for node in 0..nodes {
            let wal = S3WalManager::new(
                namespace.clone(),
                format!("node-{}", node),
                storage.clone(),
                None,
            );
            handles.push(tokio::spawn(async move {
                for seq in 0..writes_per_node {
                    let op = WalOperation::Upsert {
                        documents: vec![Document {
                            id: (node * 100 + seq) as u64,
                            vector: Some(vec![seq as f32]),
                            attributes: Default::default(),
                        }],
                    };
                    wal.append(op).await.unwrap();
                }
            }));
        }

        for handle in handles {
            handle.await.unwrap();
        }

        let reader = S3WalManager::new(namespace, "reader".to_string(), storage, None);
        let operations = reader.replay().await.unwrap();
        assert_eq!(operations.len(), (nodes * writes_per_node) as usize);
    }
}
