//! Local filesystem storage backend

use async_trait::async_trait;
use bytes::Bytes;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::{Error, Result};

use super::StorageBackend;

/// Local filesystem storage
pub struct LocalStorage {
    root_path: PathBuf,
}

impl LocalStorage {
    pub fn new(root_path: impl Into<PathBuf>) -> Result<Self> {
        let root_path = root_path.into();
        std::fs::create_dir_all(&root_path)?;
        Ok(Self { root_path })
    }

    fn resolve_path(&self, key: &str) -> PathBuf {
        self.root_path.join(key)
    }
}

#[async_trait]
impl StorageBackend for LocalStorage {
    async fn get(&self, key: &str) -> Result<Bytes> {
        let path = self.resolve_path(key);
        let data = fs::read(&path).await?;
        Ok(Bytes::from(data))
    }

    async fn put(&self, key: &str, data: Bytes) -> Result<()> {
        let path = self.resolve_path(key);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(&path, &data).await?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let path = self.resolve_path(key);
        if path.exists() {
            fs::remove_file(&path).await?;
        }
        Ok(())
    }

    async fn exists(&self, key: &str) -> Result<bool> {
        let path = self.resolve_path(key);
        Ok(path.exists())
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let prefix_path = self.resolve_path(prefix);
        let mut results = Vec::new();

        if !prefix_path.exists() {
            return Ok(results);
        }

        let mut entries = fs::read_dir(&prefix_path).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if let Ok(relative) = path.strip_prefix(&self.root_path) {
                if let Some(s) = relative.to_str() {
                    results.push(s.to_string());
                }
            }
        }

        Ok(results)
    }

    async fn get_range(&self, key: &str, start: u64, end: u64) -> Result<Bytes> {
        let path = self.resolve_path(key);
        let mut file = fs::File::open(&path).await?;

        file.seek(std::io::SeekFrom::Start(start)).await?;

        let length = (end - start) as usize;
        let mut buffer = vec![0u8; length];
        file.read_exact(&mut buffer).await?;

        Ok(Bytes::from(buffer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_local_storage() {
        let temp_dir = TempDir::new().unwrap();
        let storage = LocalStorage::new(temp_dir.path()).unwrap();

        let key = "test/file.txt";
        let data = Bytes::from("hello world");

        storage.put(key, data.clone()).await.unwrap();
        assert!(storage.exists(key).await.unwrap());

        let retrieved = storage.get(key).await.unwrap();
        assert_eq!(retrieved, data);

        storage.delete(key).await.unwrap();
        assert!(!storage.exists(key).await.unwrap());
    }
}
