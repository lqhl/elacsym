//! Vector database service

use crate::models::*;
use elacsym_builder::BuilderService;
use elacsym_core::{Error, Namespace, Result, ScoredVector, Vector, VectorId};
use elacsym_index::{QueryExecutor, SearchConfig};
use elacsym_metadata::{Metadata, MetadataService, MetadataStore, NamespaceMetadata};
use elacsym_storage::BlobStorage;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info};

/// Vector database service
pub struct VectorDBService<S: MetadataStore> {
    storage: Arc<BlobStorage>,
    metadata: Arc<MetadataService<S>>,
    builder: Arc<BuilderService<S>>,
    namespaces: Arc<RwLock<HashMap<String, NamespaceMetadata>>>,
}

impl<S: MetadataStore> VectorDBService<S> {
    pub fn new(
        storage: Arc<BlobStorage>,
        metadata: Arc<MetadataService<S>>,
        builder: Arc<BuilderService<S>>,
    ) -> Self {
        Self {
            storage,
            metadata,
            builder,
            namespaces: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Initialize service and load namespaces
    pub async fn init(&self) -> Result<()> {
        info!("Initializing VectorDB service");

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
    pub async fn create_namespace(
        &self,
        name: String,
        dimension: usize,
    ) -> Result<()> {
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

    /// Upsert vectors
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

            let vector = Vector::new(
                id,
                data.values,
                data.metadata,
                namespace.clone(),
            );

            vectors.push(vector);
        }

        let count = vectors.len();

        // Add to builder service (which will add to freshness layer)
        self.builder.add_vectors(&namespace, vectors).await?;

        info!("Upserted {} vectors to namespace {}", count, namespace);

        Ok(UpsertResponse {
            upserted_count: count,
        })
    }

    /// Query vectors
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

        // Search freshness layer first
        let freshness_layer = self.builder.get_freshness_layer(&namespace).await;
        let freshness_results = freshness_layer.search(&request.vector, &search_config).await?;

        debug!("Freshness layer returned {} results", freshness_results.len());

        // TODO: Search main index (slabs) as well
        // For now, just return freshness layer results

        let matches = freshness_results
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

    /// Fetch vectors by ID
    pub async fn fetch(&self, namespace: &str, ids: Vec<String>) -> Result<FetchResponse> {
        // Get from freshness layer
        let freshness_layer = self.builder.get_freshness_layer(namespace).await;

        let mut vectors = HashMap::new();
        for id_str in ids {
            let id = VectorId::from(id_str.clone());
            if let Some(vector) = freshness_layer.get(&id) {
                vectors.insert(
                    id_str,
                    VectorData {
                        id: Some(vector.id.to_string()),
                        values: vector.values,
                        metadata: vector.metadata,
                    },
                );
            }
        }

        Ok(FetchResponse { vectors })
    }

    /// Delete vectors by ID
    pub async fn delete(&self, namespace: &str, ids: Vec<String>) -> Result<DeleteResponse> {
        let freshness_layer = self.builder.get_freshness_layer(namespace).await;

        let mut deleted_count = 0;
        for id_str in ids {
            let id = VectorId::from(id_str);
            if freshness_layer.remove(&id).is_some() {
                deleted_count += 1;
            }
        }

        info!("Deleted {} vectors from namespace {}", deleted_count, namespace);

        Ok(DeleteResponse { deleted_count })
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
