//! Namespace management
//!
//! Namespace is the core abstraction that ties together:
//! - Manifest (metadata)
//! - Storage (S3/Local)
//! - Vector Index (RaBitQ)
//! - Segments (Parquet files)

use bytes::Bytes;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::index::VectorIndex;
use crate::manifest::{Manifest, ManifestManager};
use crate::segment::{SegmentReader, SegmentWriter};
use crate::storage::StorageBackend;
use crate::types::{Document, Schema, SegmentInfo};
use crate::{Error, Result};

/// Namespace represents a collection of documents with a shared schema
pub struct Namespace {
    name: String,
    storage: Arc<dyn StorageBackend>,
    manifest_manager: ManifestManager,

    /// The manifest (protected by RwLock for concurrent access)
    manifest: Arc<RwLock<Manifest>>,

    /// Vector index (protected by RwLock for concurrent writes)
    vector_index: Arc<RwLock<VectorIndex>>,
}

impl Namespace {
    /// Create a new namespace
    pub async fn create(
        name: String,
        schema: Schema,
        storage: Arc<dyn StorageBackend>,
    ) -> Result<Self> {
        let manifest_manager = ManifestManager::new(storage.clone());

        // Create manifest
        let manifest = manifest_manager.create(name.clone(), schema.clone()).await?;

        // Create vector index
        let vector_index = VectorIndex::new(
            schema.vector_dim,
            schema.vector_metric,
        )?;

        Ok(Self {
            name,
            storage,
            manifest_manager,
            manifest: Arc::new(RwLock::new(manifest)),
            vector_index: Arc::new(RwLock::new(vector_index)),
        })
    }

    /// Load an existing namespace
    pub async fn load(
        name: String,
        storage: Arc<dyn StorageBackend>,
    ) -> Result<Self> {
        let manifest_manager = ManifestManager::new(storage.clone());

        // Load manifest
        let manifest = manifest_manager.load(&name).await?;

        // Create vector index
        let vector_index = VectorIndex::new(
            manifest.schema.vector_dim,
            manifest.schema.vector_metric,
        )?;

        // TODO: Load existing vectors from segments into index
        // This would require reading all segments and rebuilding the index
        // For now, index will be built on first upsert

        Ok(Self {
            name,
            storage,
            manifest_manager,
            manifest: Arc::new(RwLock::new(manifest)),
            vector_index: Arc::new(RwLock::new(vector_index)),
        })
    }

    /// Upsert documents into the namespace
    pub async fn upsert(&self, documents: Vec<Document>) -> Result<usize> {
        if documents.is_empty() {
            return Ok(0);
        }

        let manifest = self.manifest.read().await;
        let schema = manifest.schema.clone();
        drop(manifest); // Release read lock

        // Validate documents against schema
        for doc in &documents {
            if let Some(ref vector) = doc.vector {
                if vector.len() != schema.vector_dim {
                    return Err(Error::InvalidSchema(format!(
                        "Vector dimension mismatch: expected {}, got {}",
                        schema.vector_dim,
                        vector.len()
                    )));
                }
            }
        }

        // Write segment to storage
        let segment_id = format!("seg_{}", chrono::Utc::now().timestamp_millis());
        let segment_path = format!("{}/segments/{}.parquet", self.name, segment_id);

        // Create segment writer
        let writer = SegmentWriter::new(schema.clone())?;
        let parquet_data = writer.write_parquet(&documents)?;

        // Upload to storage
        self.storage.put(&segment_path, parquet_data).await?;

        // Calculate ID range
        let ids: Vec<u64> = documents.iter().map(|d| d.id).collect();
        let min_id = *ids.iter().min().unwrap();
        let max_id = *ids.iter().max().unwrap();

        // Create segment info
        let segment_info = SegmentInfo {
            segment_id: segment_id.clone(),
            file_path: segment_path.clone(),
            row_count: documents.len(),
            id_range: (min_id, max_id),
            created_at: chrono::Utc::now(),
            tombstones: vec![],
        };

        // Update manifest
        let mut manifest = self.manifest.write().await;
        manifest.add_segment(segment_info);
        self.manifest_manager.save(&manifest).await?;

        // Update vector index
        let mut index = self.vector_index.write().await;
        let vectors_to_add: Vec<_> = documents
            .iter()
            .filter_map(|doc| doc.vector.as_ref().map(|v| (doc.id, v.clone())))
            .collect();

        if !vectors_to_add.is_empty() {
            let (ids, vectors): (Vec<_>, Vec<_>) = vectors_to_add.into_iter().unzip();
            index.add(&ids, &vectors)?;
        }

        Ok(documents.len())
    }

    /// Query the namespace
    pub async fn query(&self, query_vector: &[f32], top_k: usize) -> Result<Vec<(u64, f32)>> {
        // Search vector index
        let mut index = self.vector_index.write().await;
        let query_vec = query_vector.to_vec();
        let results = index.search(&query_vec, top_k)?;

        Ok(results)
    }

    /// Get the namespace schema
    pub async fn schema(&self) -> Schema {
        let manifest = self.manifest.read().await;
        manifest.schema.clone()
    }

    /// Get namespace statistics
    pub async fn stats(&self) -> crate::types::NamespaceStats {
        let manifest = self.manifest.read().await;
        manifest.stats.clone()
    }
}

/// NamespaceManager manages multiple namespaces
pub struct NamespaceManager {
    storage: Arc<dyn StorageBackend>,
    namespaces: Arc<RwLock<HashMap<String, Arc<Namespace>>>>,
}

impl NamespaceManager {
    pub fn new(storage: Arc<dyn StorageBackend>) -> Self {
        Self {
            storage,
            namespaces: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Create a new namespace
    pub async fn create_namespace(&self, name: String, schema: Schema) -> Result<Arc<Namespace>> {
        // Check if namespace already exists
        {
            let namespaces = self.namespaces.read().await;
            if namespaces.contains_key(&name) {
                return Err(Error::InvalidRequest(format!(
                    "Namespace '{}' already exists",
                    name
                )));
            }
        }

        // Create namespace
        let namespace = Arc::new(
            Namespace::create(name.clone(), schema, self.storage.clone()).await?
        );

        // Store in cache
        let mut namespaces = self.namespaces.write().await;
        namespaces.insert(name, namespace.clone());

        Ok(namespace)
    }

    /// Get or load a namespace
    pub async fn get_namespace(&self, name: &str) -> Result<Arc<Namespace>> {
        // Check cache first
        {
            let namespaces = self.namespaces.read().await;
            if let Some(ns) = namespaces.get(name) {
                return Ok(ns.clone());
            }
        }

        // Try to load from storage
        let namespace = Arc::new(
            Namespace::load(name.to_string(), self.storage.clone()).await?
        );

        // Store in cache
        let mut namespaces = self.namespaces.write().await;
        namespaces.insert(name.to_string(), namespace.clone());

        Ok(namespace)
    }

    /// List all namespaces
    pub async fn list_namespaces(&self) -> Result<Vec<String>> {
        // List all manifest files in storage
        let keys = self.storage.list("").await?;

        let namespaces: Vec<String> = keys
            .iter()
            .filter_map(|key| {
                // Extract namespace from path like "namespace/manifest.json"
                if key.ends_with("/manifest.json") {
                    key.strip_suffix("/manifest.json").map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect();

        Ok(namespaces)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::local::LocalStorage;
    use crate::types::{AttributeSchema, AttributeType, AttributeValue, DistanceMetric};
    use std::collections::HashMap;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_namespace_create_and_upsert() {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());

        let mut attributes = HashMap::new();
        attributes.insert(
            "title".to_string(),
            AttributeSchema {
                attr_type: AttributeType::String,
                indexed: false,
                full_text: true,
            },
        );

        let schema = Schema {
            vector_dim: 128,
            vector_metric: DistanceMetric::L2,
            attributes,
        };

        // Create namespace
        let ns = Namespace::create("test_ns".to_string(), schema, storage.clone())
            .await
            .unwrap();

        // Create documents
        let mut doc1_attrs = HashMap::new();
        doc1_attrs.insert(
            "title".to_string(),
            AttributeValue::String("Test Document 1".to_string()),
        );

        let doc1 = Document {
            id: 1,
            vector: Some(vec![1.0; 128]),
            attributes: doc1_attrs,
        };

        let mut doc2_attrs = HashMap::new();
        doc2_attrs.insert(
            "title".to_string(),
            AttributeValue::String("Test Document 2".to_string()),
        );

        let doc2 = Document {
            id: 2,
            vector: Some(vec![2.0; 128]),
            attributes: doc2_attrs,
        };

        // Upsert documents
        let count = ns.upsert(vec![doc1, doc2]).await.unwrap();
        assert_eq!(count, 2);

        // Check stats
        let stats = ns.stats().await;
        assert_eq!(stats.total_docs, 2);
        assert_eq!(stats.segment_count, 1);
    }

    #[tokio::test]
    async fn test_namespace_query() {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());

        let schema = Schema {
            vector_dim: 64,
            vector_metric: DistanceMetric::L2,
            attributes: HashMap::new(),
        };

        let ns = Namespace::create("test_ns".to_string(), schema, storage)
            .await
            .unwrap();

        // Add some documents
        let docs = vec![
            Document {
                id: 1,
                vector: Some(vec![1.0; 64]),
                attributes: HashMap::new(),
            },
            Document {
                id: 2,
                vector: Some(vec![2.0; 64]),
                attributes: HashMap::new(),
            },
            Document {
                id: 3,
                vector: Some(vec![3.0; 64]),
                attributes: HashMap::new(),
            },
        ];

        ns.upsert(docs).await.unwrap();

        // Query
        let query = vec![2.5; 64];
        let results = ns.query(&query, 2).await.unwrap();

        assert_eq!(results.len(), 2);
        // Should return closest vectors
        assert!(results.iter().any(|(id, _)| *id == 2 || *id == 3));
    }
}
