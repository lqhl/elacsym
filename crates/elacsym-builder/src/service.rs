//! Index builder service

use elacsym_core::{Error, PartitionId, Result, Vector};
use elacsym_index::{FreshnessLayer, IndexBuilder, IndexConfig};
use elacsym_metadata::{Metadata, MetadataService, MetadataStore};
use elacsym_storage::{BlobStorage, SlabStorage};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Configuration for the builder service
#[derive(Debug, Clone)]
pub struct BuilderConfig {
    /// Minimum vectors before building an index
    pub min_vectors: usize,

    /// Maximum vectors per slab
    pub max_vectors_per_slab: usize,

    /// Build interval in seconds
    pub build_interval_secs: u64,

    /// Index configuration
    pub index_config: IndexConfig,
}

impl Default for BuilderConfig {
    fn default() -> Self {
        Self {
            min_vectors: 1000,
            max_vectors_per_slab: 100_000,
            build_interval_secs: 60,
            index_config: IndexConfig::new(128, elacsym_core::DistanceMetric::L2),
        }
    }
}

/// Index builder service
pub struct BuilderService<S: MetadataStore> {
    config: BuilderConfig,
    storage: Arc<BlobStorage>,
    metadata: Arc<MetadataService<S>>,
    freshness_layers: Arc<RwLock<HashMap<String, Arc<FreshnessLayer>>>>,
}

impl<S: MetadataStore> BuilderService<S> {
    pub fn new(
        config: BuilderConfig,
        storage: Arc<BlobStorage>,
        metadata: Arc<MetadataService<S>>,
    ) -> Self {
        Self {
            config,
            storage,
            metadata,
            freshness_layers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get or create freshness layer for a namespace
    pub async fn get_freshness_layer(&self, namespace: &str) -> Arc<FreshnessLayer> {
        let mut layers = self.freshness_layers.write().await;

        if let Some(layer) = layers.get(namespace) {
            return Arc::clone(layer);
        }

        // Create new freshness layer
        let layer = Arc::new(FreshnessLayer::new(
            namespace.to_string(),
            self.config.index_config.clone(),
            self.config.min_vectors,
        ));

        layers.insert(namespace.to_string(), Arc::clone(&layer));

        layer
    }

    /// Add vectors to freshness layer
    pub async fn add_vectors(&self, namespace: &str, vectors: Vec<Vector>) -> Result<()> {
        let layer = self.get_freshness_layer(namespace).await;
        layer.add_batch(vectors).await?;

        // Check if we should flush
        if layer.len() >= self.config.min_vectors {
            self.flush_namespace(namespace).await?;
        }

        Ok(())
    }

    /// Flush a namespace's freshness layer and build indexes
    pub async fn flush_namespace(&self, namespace: &str) -> Result<()> {
        let layer = self.get_freshness_layer(namespace).await;

        // Get all vectors from freshness layer
        let vectors = layer.flush();

        if vectors.is_empty() {
            return Ok(());
        }

        info!(
            "Flushing {} vectors from namespace {}",
            vectors.len(),
            namespace
        );

        // Build indexes for each partition
        self.build_indexes(namespace, vectors).await?;

        Ok(())
    }

    /// Build indexes from vectors
    async fn build_indexes(&self, namespace: &str, vectors: Vec<Vector>) -> Result<()> {
        // Get namespace metadata
        let ns_metadata = self.metadata.get_namespace(namespace).await?;

        if ns_metadata.is_none() {
            return Err(Error::NamespaceNotFound(namespace.to_string()));
        }

        // Group vectors by partition
        // For now, we'll use a simple partitioning strategy
        // In production, you'd use the PartitionManager to assign partitions
        let partition_id: PartitionId = 0; // Simplified for now

        // Split into slabs if needed
        let slabs = self.split_into_slabs(namespace, partition_id, vectors)?;

        // Upload slabs to storage
        for slab in slabs {
            self.storage
                .put_slab(&slab)
                .await
                .map_err(|e| Error::Storage(anyhow::anyhow!("Failed to upload slab: {}", e)))?;

            // Register slab in metadata
            self.metadata
                .register_slab(namespace, partition_id, slab.metadata.id)
                .await?;

            info!(
                "Uploaded slab {} with {} vectors",
                slab.metadata.id,
                slab.vector_count()
            );
        }

        Ok(())
    }

    /// Split vectors into slabs
    fn split_into_slabs(
        &self,
        namespace: &str,
        partition_id: PartitionId,
        vectors: Vec<Vector>,
    ) -> Result<Vec<elacsym_core::Slab>> {
        let mut slabs = Vec::new();

        for chunk in vectors.chunks(self.config.max_vectors_per_slab) {
            let builder = IndexBuilder::new(self.config.index_config.clone())?;
            let slab = builder.build(namespace.to_string(), partition_id, chunk)?;
            slabs.push(slab);
        }

        Ok(slabs)
    }

    /// Run periodic flush
    pub async fn run_periodic_flush(&self) {
        let interval = tokio::time::Duration::from_secs(self.config.build_interval_secs);

        loop {
            tokio::time::sleep(interval).await;

            // Flush all namespaces
            let namespaces: Vec<String> = {
                let layers = self.freshness_layers.read().await;
                layers.keys().cloned().collect()
            };

            for namespace in namespaces {
                if let Err(e) = self.flush_namespace(&namespace).await {
                    warn!("Failed to flush namespace {}: {}", namespace, e);
                }
            }
        }
    }
}
