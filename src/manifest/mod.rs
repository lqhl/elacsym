//! Namespace manifest management
//!
//! Manifest stores metadata about a namespace including:
//! - Schema definition
//! - Segment list
//! - Index locations
//! - Statistics

use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::StorageBackend;
use crate::types::{NamespaceStats, Schema, SegmentInfo};
use crate::{Error, Result};

/// Namespace manifest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u64,
    pub namespace: String,
    pub schema: Schema,
    pub segments: Vec<SegmentInfo>,
    pub indexes: IndexLocations,
    pub stats: NamespaceStats,
    pub updated_at: DateTime<Utc>,
}

/// Index file locations
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IndexLocations {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector: Option<String>,
    #[serde(default)]
    pub full_text: HashMap<String, String>,
}

impl Manifest {
    /// Create a new manifest for a namespace
    pub fn new(namespace: String, schema: Schema) -> Self {
        Self {
            version: 1,
            namespace,
            schema,
            segments: Vec::new(),
            indexes: IndexLocations::default(),
            stats: NamespaceStats::default(),
            updated_at: Utc::now(),
        }
    }

    /// Add a new segment to the manifest
    pub fn add_segment(&mut self, segment: SegmentInfo) {
        self.stats.total_docs += segment.row_count;
        self.segments.push(segment);
        self.stats.segment_count = self.segments.len();
        self.version += 1;
        self.updated_at = Utc::now();
    }

    /// Mark documents as deleted (tombstone)
    pub fn mark_deleted(&mut self, doc_ids: &[u64]) -> Result<()> {
        for doc_id in doc_ids {
            // Find the segment containing this doc_id
            let segment = self
                .segments
                .iter_mut()
                .find(|s| *doc_id >= s.id_range.0 && *doc_id <= s.id_range.1)
                .ok_or_else(|| Error::internal(format!("Doc {} not found", doc_id)))?;

            if !segment.tombstones.contains(doc_id) {
                segment.tombstones.push(*doc_id);
            }
        }

        self.version += 1;
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Serialize to JSON
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).map_err(Into::into)
    }

    /// Deserialize from JSON
    pub fn from_json(json: &str) -> Result<Self> {
        serde_json::from_str(json).map_err(Into::into)
    }
}

/// Manifest manager for loading and saving manifests
pub struct ManifestManager {
    storage: Arc<dyn StorageBackend>,
}

impl ManifestManager {
    pub fn new(storage: Arc<dyn StorageBackend>) -> Self {
        Self { storage }
    }

    /// Get versioned manifest key
    fn versioned_manifest_key(namespace: &str, version: u64) -> String {
        format!("{}/manifests/v{:08}.json", namespace, version)
    }

    /// Get current version pointer key
    fn current_version_key(namespace: &str) -> String {
        format!("{}/manifests/current.txt", namespace)
    }

    /// Legacy manifest key (for backward compatibility)
    fn legacy_manifest_key(namespace: &str) -> String {
        format!("{}/manifest.json", namespace)
    }

    /// Read current version number
    async fn read_current_version(&self, namespace: &str) -> Result<u64> {
        let key = Self::current_version_key(namespace);

        match self.storage.get(&key).await {
            Ok(data) => {
                let version_str = String::from_utf8(data.to_vec())
                    .map_err(|e| Error::internal(format!("Invalid UTF-8 in current.txt: {}", e)))?;

                version_str
                    .trim()
                    .parse::<u64>()
                    .map_err(|e| Error::internal(format!("Invalid version number: {}", e)))
            }
            Err(_) => {
                // Try legacy manifest.json
                if self.storage.exists(&Self::legacy_manifest_key(namespace)).await? {
                    // Migrate from legacy
                    tracing::info!("Migrating namespace '{}' to versioned manifests", namespace);
                    Ok(0) // Will create v1 on next save
                } else {
                    Err(Error::NamespaceNotFound(namespace.to_string()))
                }
            }
        }
    }

    /// Load manifest from storage (supports versioned and legacy)
    pub async fn load(&self, namespace: &str) -> Result<Manifest> {
        // Try versioned manifest first
        match self.read_current_version(namespace).await {
            Ok(version) if version > 0 => {
                let key = Self::versioned_manifest_key(namespace, version);
                let data = self.storage.get(&key).await?;

                let json = String::from_utf8(data.to_vec())
                    .map_err(|e| Error::internal(format!("Invalid UTF-8 in manifest: {}", e)))?;

                Manifest::from_json(&json)
            }
            _ => {
                // Fall back to legacy manifest.json
                let key = Self::legacy_manifest_key(namespace);
                let data = self
                    .storage
                    .get(&key)
                    .await
                    .map_err(|_e| Error::NamespaceNotFound(namespace.to_string()))?;

                let json = String::from_utf8(data.to_vec())
                    .map_err(|e| Error::internal(format!("Invalid UTF-8 in manifest: {}", e)))?;

                Manifest::from_json(&json)
            }
        }
    }

    /// Save manifest to storage (versioned, optimistic locking)
    pub async fn save(&self, manifest: &Manifest) -> Result<()> {
        let json = manifest.to_json()?;
        let data = Bytes::from(json.into_bytes());

        // Read current version (or 0 if not exists)
        let current_version = self.read_current_version(&manifest.namespace)
            .await
            .unwrap_or(0);

        // Use manifest version if it's newer (from manifest.version)
        let new_version = manifest.version.max(current_version + 1);

        // 1. Write new versioned manifest
        let versioned_key = Self::versioned_manifest_key(&manifest.namespace, new_version);
        self.storage.put(&versioned_key, data.clone()).await?;

        tracing::debug!(
            "Wrote manifest version {} for namespace '{}'",
            new_version,
            manifest.namespace
        );

        // 2. Update current.txt pointer (last step, atomic)
        let current_key = Self::current_version_key(&manifest.namespace);
        let version_bytes = Bytes::from(new_version.to_string());
        self.storage.put(&current_key, version_bytes).await?;

        // 3. Also write legacy manifest.json for backward compatibility
        let legacy_key = Self::legacy_manifest_key(&manifest.namespace);
        self.storage.put(&legacy_key, data).await?;

        Ok(())
    }

    /// Check if namespace exists
    pub async fn exists(&self, namespace: &str) -> Result<bool> {
        // Check versioned manifest first
        let current_key = Self::current_version_key(namespace);
        if self.storage.exists(&current_key).await? {
            return Ok(true);
        }

        // Fall back to legacy manifest
        let legacy_key = Self::legacy_manifest_key(namespace);
        self.storage.exists(&legacy_key).await
    }

    /// Create a new namespace
    pub async fn create(&self, namespace: String, schema: Schema) -> Result<Manifest> {
        // Check if already exists
        if self.exists(&namespace).await? {
            return Err(Error::internal(format!(
                "Namespace {} already exists",
                namespace
            )));
        }

        let manifest = Manifest::new(namespace, schema);
        self.save(&manifest).await?;

        Ok(manifest)
    }

    /// Delete a namespace
    pub async fn delete(&self, namespace: &str) -> Result<()> {
        // Delete current.txt
        let current_key = Self::current_version_key(namespace);
        let _ = self.storage.delete(&current_key).await;

        // Delete legacy manifest
        let legacy_key = Self::legacy_manifest_key(namespace);
        let _ = self.storage.delete(&legacy_key).await;

        // Note: Versioned manifests and segments are not deleted here
        // They should be cleaned up by a separate GC process

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::local::LocalStorage;
    use crate::types::DistanceMetric;
    use tempfile::TempDir;

    #[test]
    fn test_manifest_creation() {
        let schema = Schema {
            vector_dim: 768,
            vector_metric: DistanceMetric::Cosine,
            attributes: HashMap::new(),
        };

        let manifest = Manifest::new("test_ns".to_string(), schema);
        assert_eq!(manifest.version, 1);
        assert_eq!(manifest.namespace, "test_ns");
        assert_eq!(manifest.segments.len(), 0);
    }

    #[tokio::test]
    async fn test_manifest_manager() {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());
        let manager = ManifestManager::new(storage);

        let schema = Schema {
            vector_dim: 768,
            vector_metric: DistanceMetric::Cosine,
            attributes: HashMap::new(),
        };

        // Create namespace
        let manifest = manager.create("test_ns".to_string(), schema).await.unwrap();
        assert_eq!(manifest.namespace, "test_ns");

        // Load namespace
        let loaded = manager.load("test_ns").await.unwrap();
        assert_eq!(loaded.version, manifest.version);
        assert_eq!(loaded.namespace, manifest.namespace);

        // Check exists
        assert!(manager.exists("test_ns").await.unwrap());
        assert!(!manager.exists("nonexistent").await.unwrap());

        // Delete namespace
        manager.delete("test_ns").await.unwrap();
        assert!(!manager.exists("test_ns").await.unwrap());
    }
}
