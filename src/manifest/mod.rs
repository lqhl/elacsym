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

    /// Get manifest key for a namespace
    fn manifest_key(namespace: &str) -> String {
        format!("{}/manifest.json", namespace)
    }

    /// Load manifest from storage
    pub async fn load(&self, namespace: &str) -> Result<Manifest> {
        let key = Self::manifest_key(namespace);

        let data = self
            .storage
            .get(&key)
            .await
            .map_err(|_e| Error::NamespaceNotFound(namespace.to_string()))?;

        let json = String::from_utf8(data.to_vec())
            .map_err(|e| Error::internal(format!("Invalid UTF-8 in manifest: {}", e)))?;

        Manifest::from_json(&json)
    }

    /// Save manifest to storage (atomic)
    pub async fn save(&self, manifest: &Manifest) -> Result<()> {
        let json = manifest.to_json()?;
        let data = Bytes::from(json.into_bytes());

        let key = Self::manifest_key(&manifest.namespace);

        // Write to temporary location first
        let temp_key = format!("{}.tmp", key);
        self.storage.put(&temp_key, data.clone()).await?;

        // Atomic rename (for S3, this is just overwrite)
        self.storage.put(&key, data).await?;

        // Clean up temp file
        let _ = self.storage.delete(&temp_key).await;

        Ok(())
    }

    /// Check if namespace exists
    pub async fn exists(&self, namespace: &str) -> Result<bool> {
        let key = Self::manifest_key(namespace);
        self.storage.exists(&key).await
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
        let key = Self::manifest_key(namespace);
        self.storage.delete(&key).await
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
