//! S3-only Vector Database Service
//!
//! This service implementation uses only S3 as the data source:
//! - S3MetadataService for metadata
//! - S3WAL for write-ahead log
//! - ParallelQueryExecutor for distributed queries
//! - DistributedBuilder for coordination-free indexing
//!
//! No RocksDB, etcd, or Kafka dependencies.

use crate::models::*;
use elacsym_builder::{DistributedBuilder, DistributedBuilderConfig};
use elacsym_core::{Error, Namespace, Result, Vector, VectorId};
use elacsym_index::{ParallelQueryExecutor, SearchConfig};
use elacsym_metadata::s3::S3MetadataService;
use elacsym_metadata::NamespaceMetadata;
use elacsym_storage::{BlobStorage, S3WAL, WALBatch, WALDelete};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info};

/// S3-only Vector Database Service
pub struct S3VectorDBService {
    storage: Arc<BlobStorage>,
    metadata: Arc<S3MetadataService>,
    wal: Arc<S3WAL>,
    builder: Arc<DistributedBuilder>,
    namespaces: Arc<RwLock<HashMap<String, NamespaceMetadata>>>,
}

impl S3VectorDBService {
    pub fn new(
        storage: Arc<BlobStorage>,
        metadata: Arc<S3MetadataService>,
        wal: Arc<S3WAL>,
        builder: Arc<DistributedBuilder>,
    ) -> Self {
        Self {
            storage,
            metadata,
            wal,
            builder,
            namespaces: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Initialize service and load namespaces
    pub async fn init(&self) -> Result<()> {
        info!("Initializing S3-only VectorDB service");

        // Load all namespaces
        let namespace_names = self.metadata.list_namespaces().await?;

        let mut namespaces = self.namespaces.write();
        for name in namespace_names {
            if let Some(ns_metadata) = self.metadata.get_namespace(&name).await? {
                namespaces.insert(name, ns_metadata);
            }
        }

        info!("Loaded {} namespaces", namespaces.len());

        Ok(())
    }

    /// Create a new namespace
    pub async fn create_namespace(&self, name: String, dimension: usize) -> Result<()> {
        if !Namespace::is_valid(&name) {
            return Err(Error::InvalidNamespace(name));
        }

        let metadata = NamespaceMetadata {
            name: name.clone(),
            dimension,
            vector_count: 0,
            partition_count: 0,
        };

        self.metadata.upsert_namespace(metadata.clone()).await?;

        let mut namespaces = self.namespaces.write();
        namespaces.insert(name.clone(), metadata);

        info!("Created namespace: {}", name);

        Ok(())
    }

    /// Upsert vectors (batch write to WAL)
    pub async fn upsert(&self, request: UpsertRequest) -> Result<UpsertResponse> {
        let namespace = if request.namespace.is_empty() {
            "default".to_string()
        } else {
            request.namespace
        };

        // Verify namespace exists
        self.ensure_namespace_exists(&namespace).await?;

        // Convert to internal Vector format
        let mut vectors = Vec::new();
        for data in request.vectors {
            let id = data.id.map(VectorId::from).unwrap_or_else(VectorId::generate);

            let vector = Vector::new(id, data.values, data.metadata, namespace.clone());

            vectors.push(vector);
        }

        let count = vectors.len();

        // Write to WAL (batch operation)
        let batch = WALBatch::new(namespace.clone(), vectors);
        self.wal.write_batch(batch).await?;

        info!("Wrote batch of {} vectors to WAL for namespace {}", count, namespace);

        Ok(UpsertResponse {
            upserted_count: count,
        })
    }

    /// Query vectors using parallel execution
    pub async fn query(&self, request: QueryRequest) -> Result<QueryResponse> {
        let namespace = if request.namespace.is_empty() {
            "default".to_string()
        } else {
            request.namespace
        };

        // Verify namespace exists
        let ns_metadata = self.ensure_namespace_exists(&namespace).await?;

        // Validate query dimension
        if request.vector.len() != ns_metadata.dimension {
            return Err(Error::DimensionMismatch {
                expected: ns_metadata.dimension,
                actual: request.vector.len(),
            });
        }

        let search_config = SearchConfig {
            top_k: request.top_k,
            nprobe: 16, // TODO: Make configurable
            include_vectors: request.include_values,
            include_metadata: request.include_metadata,
        };

        // Use parallel query executor
        let executor = ParallelQueryExecutor::new(
            Arc::clone(&self.storage),
            Arc::clone(&self.wal),
            search_config,
        );

        let results = executor.query(&namespace, &request.vector).await?;

        debug!("Query returned {} results", results.len());

        let matches = results
            .into_iter()
            .map(|sv| ScoredVectorData {
                id: sv.id.to_string(),
                score: sv.score,
                values: if request.include_values {
                    sv.vector.map(|v| v.values)
                } else {
                    None
                },
                metadata: if request.include_metadata {
                    sv.vector.and_then(|v| Some(v.metadata))
                } else {
                    None
                },
            })
            .collect();

        Ok(QueryResponse { matches })
    }

    /// Fetch vectors by ID (from WAL)
    pub async fn fetch(&self, namespace: &str, ids: Vec<String>) -> Result<FetchResponse> {
        // Get all unindexed vectors from WAL
        let vectors = self.wal.get_unindexed_vectors(namespace).await?;

        let mut result_vectors = HashMap::new();

        for id_str in ids {
            let id = VectorId::from(id_str.clone());

            // Search in unindexed vectors
            if let Some(vector) = vectors.iter().find(|v| v.id == id) {
                result_vectors.insert(
                    id_str,
                    VectorData {
                        id: Some(vector.id.to_string()),
                        values: vector.values.clone(),
                        metadata: vector.metadata.clone(),
                    },
                );
            }
        }

        Ok(FetchResponse {
            vectors: result_vectors,
        })
    }

    /// Delete vectors by ID (write to WAL)
    pub async fn delete(&self, namespace: &str, ids: Vec<String>) -> Result<DeleteResponse> {
        let vector_ids: Vec<VectorId> = ids.into_iter().map(VectorId::from).collect();

        let count = vector_ids.len();

        // Write delete entry to WAL
        let delete = WALDelete::new(namespace.to_string(), vector_ids);
        self.wal.write_delete(delete).await?;

        info!("Wrote delete entry for {} vectors to WAL in namespace {}", count, namespace);

        Ok(DeleteResponse {
            deleted_count: count,
        })
    }

    /// Trigger index building for a namespace
    pub async fn build_indexes(&self, namespace: &str) -> Result<BuildIndexResponse> {
        info!("Triggering index build for namespace: {}", namespace);

        let slab_ids = self.builder.build_indexes(namespace).await?;

        info!("Built {} slabs for namespace: {}", slab_ids.len(), namespace);

        Ok(BuildIndexResponse {
            slabs_built: slab_ids.len(),
        })
    }

    /// Run a full build cycle (build + mark as indexed)
    pub async fn run_build_cycle(&self, namespace: &str) -> Result<BuildIndexResponse> {
        info!("Running full build cycle for namespace: {}", namespace);

        self.builder.run_build_cycle(namespace).await?;

        Ok(BuildIndexResponse { slabs_built: 0 })
    }

    /// Ensure namespace exists, return metadata
    async fn ensure_namespace_exists(&self, namespace: &str) -> Result<NamespaceMetadata> {
        let namespaces = self.namespaces.read();
        if let Some(metadata) = namespaces.get(namespace) {
            Ok(metadata.clone())
        } else {
            drop(namespaces);

            // Try to load from metadata service
            if let Some(metadata) = self.metadata.get_namespace(namespace).await? {
                let mut namespaces = self.namespaces.write();
                namespaces.insert(namespace.to_string(), metadata.clone());
                Ok(metadata)
            } else {
                Err(Error::NamespaceNotFound(namespace.to_string()))
            }
        }
    }
}

/// Build index response
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct BuildIndexResponse {
    pub slabs_built: usize,
}

/// Upsert response
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct UpsertResponse {
    pub upserted_count: usize,
}

/// Fetch response
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct FetchResponse {
    pub vectors: HashMap<String, VectorData>,
}

/// Delete response
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct DeleteResponse {
    pub deleted_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use elacsym_metadata::s3::S3MetadataStore;
    use elacsym_storage::{BlobStorageConfig, StorageBackend};
    use object_store::memory::InMemory;

    #[tokio::test]
    async fn test_s3_service_create_namespace() {
        let store = Arc::new(InMemory::new());

        let storage = Arc::new(
            BlobStorage::new(BlobStorageConfig {
                base_path: "test".to_string(),
                backend: StorageBackend::Memory,
            })
            .await
            .unwrap(),
        );

        let metadata_store = Arc::new(S3MetadataStore::new(store.clone(), "metadata".to_string()));
        let metadata = Arc::new(S3MetadataService::new(metadata_store));

        let wal = Arc::new(S3WAL::new(store, "data".to_string()));

        let builder_config = DistributedBuilderConfig::default();
        let builder = Arc::new(DistributedBuilder::new(
            builder_config,
            Arc::clone(&storage),
            Arc::clone(&metadata),
            Arc::clone(&wal),
        ));

        let service = S3VectorDBService::new(storage, metadata, wal, builder);

        service.create_namespace("test".to_string(), 128).await.unwrap();

        let ns_metadata = service.ensure_namespace_exists("test").await.unwrap();
        assert_eq!(ns_metadata.name, "test");
        assert_eq!(ns_metadata.dimension, 128);
    }
}
