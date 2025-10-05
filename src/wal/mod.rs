//! Write-Ahead Log (WAL) for durability guarantees
//!
//! Provides crash-safe writes by logging operations before they're committed.
//! Inspired by Turbopuffer's WAL design.

use bytes::{BufMut, BytesMut};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::types::{Document, DocId};
use crate::{Error, Result};

/// WAL operation types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WalOperation {
    /// Insert or update documents
    Upsert { documents: Vec<Document> },
    /// Delete documents
    Delete { ids: Vec<DocId> },
    /// Commit a batch of operations
    Commit { batch_id: u64 },
}

/// WAL entry with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalEntry {
    /// Sequence number (monotonically increasing)
    pub sequence: u64,
    /// Timestamp
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Operation
    pub operation: WalOperation,
}

/// WAL file format:
/// - Magic bytes: "EWAL" (4 bytes)
/// - Version: u32 (4 bytes)
/// - Entries: [Entry]*
///
/// Each entry:
/// - Length: u32 (4 bytes) - length of serialized entry
/// - Data: serialized WalEntry (msgpack)
/// - CRC32: u32 (4 bytes) - checksum of length + data

const WAL_MAGIC: &[u8; 4] = b"EWAL";
const WAL_VERSION: u32 = 1;

/// Write-Ahead Log manager
pub struct WalManager {
    /// Directory for WAL files
    wal_dir: PathBuf,
    /// Current WAL file
    current_file: File,
    /// Current WAL file path
    current_path: PathBuf,
    /// Current file sequence number
    file_sequence: u64,
    /// Next entry sequence number
    next_sequence: u64,
    /// Maximum WAL file size before rotation (default: 100MB)
    max_file_size: u64,
}

impl WalManager {
    /// Create a new WAL manager
    pub async fn new<P: AsRef<Path>>(wal_dir: P) -> Result<Self> {
        let wal_dir = wal_dir.as_ref().to_path_buf();

        // Create WAL directory if it doesn't exist
        tokio::fs::create_dir_all(&wal_dir)
            .await
            .map_err(|e| Error::internal(format!("Failed to create WAL directory: {}", e)))?;

        // Create initial WAL file
        let current_path = wal_dir.join("wal_000000.log");
        let mut current_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&current_path)
            .await
            .map_err(|e| Error::internal(format!("Failed to open WAL file: {}", e)))?;

        // Write header if file is empty
        let metadata = current_file
            .metadata()
            .await
            .map_err(|e| Error::internal(format!("Failed to get file metadata: {}", e)))?;

        let next_sequence = if metadata.len() == 0 {
            // New file - write header
            current_file
                .write_all(WAL_MAGIC)
                .await
                .map_err(|e| Error::internal(format!("Failed to write WAL magic: {}", e)))?;
            current_file
                .write_u32(WAL_VERSION)
                .await
                .map_err(|e| Error::internal(format!("Failed to write WAL version: {}", e)))?;
            current_file
                .flush()
                .await
                .map_err(|e| Error::internal(format!("Failed to flush WAL: {}", e)))?;
            0
        } else {
            // Existing file - read last sequence
            Self::read_last_sequence(&current_path).await?
        };

        Ok(Self {
            wal_dir,
            current_file,
            current_path,
            file_sequence: 0,
            next_sequence,
            max_file_size: 100 * 1024 * 1024, // 100MB default
        })
    }

    /// Check if WAL file should be rotated
    async fn should_rotate(&self) -> Result<bool> {
        let metadata = self.current_file.metadata().await
            .map_err(|e| Error::internal(format!("Failed to get file metadata: {}", e)))?;
        Ok(metadata.len() >= self.max_file_size)
    }

    /// Rotate to a new WAL file
    async fn rotate(&mut self) -> Result<()> {
        // Increment file sequence
        self.file_sequence += 1;

        // Create new file path
        let new_path = self.wal_dir.join(format!("wal_{:06}.log", self.file_sequence));

        // Close current file (it will be dropped)
        // Open new file
        let mut new_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&new_path)
            .await
            .map_err(|e| Error::internal(format!("Failed to create new WAL file: {}", e)))?;

        // Write header to new file
        new_file.write_all(WAL_MAGIC).await
            .map_err(|e| Error::internal(format!("Failed to write WAL magic: {}", e)))?;
        new_file.write_u32(WAL_VERSION).await
            .map_err(|e| Error::internal(format!("Failed to write WAL version: {}", e)))?;
        new_file.flush().await
            .map_err(|e| Error::internal(format!("Failed to flush WAL: {}", e)))?;

        // Update current file and path
        self.current_file = new_file;
        self.current_path = new_path;

        // Cleanup old WAL files (keep only last 5)
        self.cleanup_old_wal_files().await?;

        Ok(())
    }

    /// Cleanup old WAL files, keeping only the most recent N files
    async fn cleanup_old_wal_files(&self) -> Result<()> {
        const MAX_WAL_FILES: usize = 5;

        // List all WAL files
        let mut entries = tokio::fs::read_dir(&self.wal_dir).await
            .map_err(|e| Error::internal(format!("Failed to read WAL directory: {}", e)))?;

        let mut wal_files = Vec::new();
        while let Some(entry) = entries.next_entry().await
            .map_err(|e| Error::internal(format!("Failed to read directory entry: {}", e)))? {
            let path = entry.path();
            if let Some(name) = path.file_name() {
                if let Some(name_str) = name.to_str() {
                    if name_str.starts_with("wal_") && name_str.ends_with(".log") {
                        wal_files.push(path);
                    }
                }
            }
        }

        // Sort by file name (which includes sequence number)
        wal_files.sort();

        // Delete old files if we have more than MAX_WAL_FILES
        if wal_files.len() > MAX_WAL_FILES {
            let files_to_delete = &wal_files[..wal_files.len() - MAX_WAL_FILES];
            for file in files_to_delete {
                if let Err(e) = tokio::fs::remove_file(file).await {
                    tracing::warn!("Failed to delete old WAL file {:?}: {}", file, e);
                }
            }
        }

        Ok(())
    }

    /// Append an operation to the WAL
    pub async fn append(&mut self, operation: WalOperation) -> Result<u64> {
        // Check if rotation is needed
        if self.should_rotate().await? {
            self.rotate().await?;
        }

        let entry = WalEntry {
            sequence: self.next_sequence,
            timestamp: chrono::Utc::now(),
            operation,
        };

        // Serialize entry using msgpack
        let data = rmp_serde::to_vec(&entry)
            .map_err(|e| Error::internal(format!("Failed to serialize WAL entry: {}", e)))?;

        // Create buffer for entry
        let mut buffer = BytesMut::with_capacity(4 + data.len() + 4);
        buffer.put_u32(data.len() as u32);
        buffer.put_slice(&data);

        // Calculate CRC32
        let crc = crc32fast::hash(&buffer);
        buffer.put_u32(crc);

        // Write to file
        self.current_file
            .write_all(&buffer)
            .await
            .map_err(|e| Error::internal(format!("Failed to write WAL entry: {}", e)))?;

        // Flush to ensure durability
        self.current_file
            .flush()
            .await
            .map_err(|e| Error::internal(format!("Failed to flush WAL: {}", e)))?;

        let seq = self.next_sequence;
        self.next_sequence += 1;
        Ok(seq)
    }

    /// Sync the WAL to disk
    pub async fn sync(&mut self) -> Result<()> {
        self.current_file
            .sync_all()
            .await
            .map_err(|e| Error::internal(format!("Failed to sync WAL: {}", e)))
    }

    /// Read all entries from the WAL
    pub async fn read_all(&self) -> Result<Vec<WalEntry>> {
        Self::read_wal_file(&self.current_path).await
    }

    /// Replay WAL entries (for crash recovery)
    ///
    /// Returns the list of operations that need to be replayed.
    /// After replaying, caller should call truncate() to clear the WAL.
    pub async fn replay(&self) -> Result<Vec<WalOperation>> {
        let entries = self.read_all().await?;
        Ok(entries.into_iter().map(|e| e.operation).collect())
    }

    /// Read last sequence number from WAL file
    async fn read_last_sequence(path: &Path) -> Result<u64> {
        let entries = Self::read_wal_file(path).await?;
        Ok(entries.last().map(|e| e.sequence + 1).unwrap_or(0))
    }

    /// Read all entries from a WAL file
    ///
    /// Gracefully handles corrupted entries by logging warnings and continuing.
    /// This ensures partial WAL recovery is possible even with corruption.
    async fn read_wal_file(path: &Path) -> Result<Vec<WalEntry>> {
        let mut file = File::open(path)
            .await
            .map_err(|e| Error::internal(format!("Failed to open WAL file: {}", e)))?;

        // Read and verify header
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic)
            .await
            .map_err(|e| Error::internal(format!("Failed to read WAL magic: {}", e)))?;

        if &magic != WAL_MAGIC {
            return Err(Error::internal("Invalid WAL file: bad magic bytes"));
        }

        let version = file
            .read_u32()
            .await
            .map_err(|e| Error::internal(format!("Failed to read WAL version: {}", e)))?;

        if version != WAL_VERSION {
            return Err(Error::internal(format!(
                "Unsupported WAL version: {}",
                version
            )));
        }

        // Read entries with error recovery
        let mut entries = Vec::new();
        let mut entry_index = 0;
        loop {
            // Read length
            let length = match file.read_u32().await {
                Ok(len) => len,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    // Normal EOF - we're done
                    break;
                }
                Err(e) => {
                    // Unexpected error reading length
                    tracing::warn!("WAL entry {} corrupted (failed to read length): {}. Stopping recovery at this point.", entry_index, e);
                    break;
                }
            };

            // Sanity check: reject unreasonably large entries (>100MB)
            if length > 100 * 1024 * 1024 {
                tracing::warn!("WAL entry {} has unreasonable length: {} bytes. Stopping recovery.", entry_index, length);
                break;
            }

            // Read data
            let mut data = vec![0u8; length as usize];
            if let Err(e) = file.read_exact(&mut data).await {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    // Truncated entry - log and stop
                    tracing::warn!("WAL entry {} truncated (expected {} bytes). Stopping recovery.", entry_index, length);
                } else {
                    tracing::warn!("WAL entry {} corrupted (failed to read data): {}. Stopping recovery.", entry_index, e);
                }
                break;
            }

            // Read CRC
            let stored_crc = match file.read_u32().await {
                Ok(crc) => crc,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    // Truncated CRC
                    tracing::warn!("WAL entry {} missing CRC. Stopping recovery.", entry_index);
                    break;
                }
                Err(e) => {
                    tracing::warn!("WAL entry {} corrupted (failed to read CRC): {}. Stopping recovery.", entry_index, e);
                    break;
                }
            };

            // Verify CRC
            let mut crc_data = BytesMut::with_capacity(4 + data.len());
            crc_data.put_u32(length);
            crc_data.put_slice(&data);
            let calculated_crc = crc32fast::hash(&crc_data);

            if calculated_crc != stored_crc {
                // CRC mismatch - log and skip this entry
                tracing::warn!(
                    "WAL entry {} CRC mismatch (expected: {}, got: {}). Skipping corrupted entry.",
                    entry_index, stored_crc, calculated_crc
                );
                entry_index += 1;
                continue;
            }

            // Deserialize entry
            let entry: WalEntry = match rmp_serde::from_slice(&data) {
                Ok(e) => e,
                Err(e) => {
                    // Deserialization failed - log and skip
                    tracing::warn!("WAL entry {} failed to deserialize: {}. Skipping.", entry_index, e);
                    entry_index += 1;
                    continue;
                }
            };

            entries.push(entry);
            entry_index += 1;
        }

        if entry_index > 0 && entries.is_empty() {
            tracing::warn!("All {} WAL entries were corrupted. No operations recovered.", entry_index);
        } else if entry_index > entries.len() {
            tracing::warn!(
                "Recovered {}/{} WAL entries. {} entries were corrupted or truncated.",
                entries.len(), entry_index, entry_index - entries.len()
            );
        }

        Ok(entries)
    }

    /// Truncate the WAL (after successful persistence)
    pub async fn truncate(&mut self) -> Result<()> {
        // Close current file
        drop(std::mem::replace(
            &mut self.current_file,
            // Temporary placeholder
            OpenOptions::new()
                .create(true)
                .append(true)
                .open("/dev/null")
                .await
                .map_err(|e| Error::internal(format!("Failed to open /dev/null: {}", e)))?,
        ));

        // Remove old file
        tokio::fs::remove_file(&self.current_path)
            .await
            .map_err(|e| Error::internal(format!("Failed to remove WAL file: {}", e)))?;

        // Create new file
        self.next_sequence = 0;
        let mut new_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.current_path)
            .await
            .map_err(|e| Error::internal(format!("Failed to create new WAL file: {}", e)))?;

        // Write header
        new_file
            .write_all(WAL_MAGIC)
            .await
            .map_err(|e| Error::internal(format!("Failed to write WAL magic: {}", e)))?;
        new_file
            .write_u32(WAL_VERSION)
            .await
            .map_err(|e| Error::internal(format!("Failed to write WAL version: {}", e)))?;
        new_file
            .flush()
            .await
            .map_err(|e| Error::internal(format!("Failed to flush WAL: {}", e)))?;

        self.current_file = new_file;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_wal_basic() {
        let temp_dir = TempDir::new().unwrap();
        let mut wal = WalManager::new(temp_dir.path()).await.unwrap();

        // Append operation
        let doc = Document {
            id: 1,
            vector: Some(vec![0.1, 0.2, 0.3]),
            attributes: Default::default(),
        };

        let seq = wal
            .append(WalOperation::Upsert {
                documents: vec![doc],
            })
            .await
            .unwrap();

        assert_eq!(seq, 0);

        // Read back
        let entries = wal.read_all().await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].sequence, 0);
    }

    #[tokio::test]
    async fn test_wal_multiple_entries() {
        let temp_dir = TempDir::new().unwrap();
        let mut wal = WalManager::new(temp_dir.path()).await.unwrap();

        // Append multiple operations
        for i in 0..10 {
            let doc = Document {
                id: i,
                vector: Some(vec![i as f32]),
                attributes: Default::default(),
            };

            wal.append(WalOperation::Upsert {
                documents: vec![doc],
            })
            .await
            .unwrap();
        }

        // Read back
        let entries = wal.read_all().await.unwrap();
        assert_eq!(entries.len(), 10);
    }

    #[tokio::test]
    async fn test_wal_truncate() {
        let temp_dir = TempDir::new().unwrap();
        let mut wal = WalManager::new(temp_dir.path()).await.unwrap();

        // Append operation
        let doc = Document {
            id: 1,
            vector: Some(vec![0.1]),
            attributes: Default::default(),
        };

        wal.append(WalOperation::Upsert {
            documents: vec![doc],
        })
        .await
        .unwrap();

        // Truncate
        wal.truncate().await.unwrap();

        // Should be empty
        let entries = wal.read_all().await.unwrap();
        assert_eq!(entries.len(), 0);
    }

    #[tokio::test]
    async fn test_wal_recovery() {
        let temp_dir = TempDir::new().unwrap();

        // Write some entries
        {
            let mut wal = WalManager::new(temp_dir.path()).await.unwrap();
            for i in 0..5 {
                let doc = Document {
                    id: i,
                    vector: Some(vec![i as f32]),
                    attributes: Default::default(),
                };

                wal.append(WalOperation::Upsert {
                    documents: vec![doc],
                })
                .await
                .unwrap();
            }
        } // Drop WAL

        // Reopen and verify recovery
        let wal = WalManager::new(temp_dir.path()).await.unwrap();
        assert_eq!(wal.next_sequence, 5);

        let entries = wal.read_all().await.unwrap();
        assert_eq!(entries.len(), 5);
    }
}
