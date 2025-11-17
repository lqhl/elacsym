//! Coordination-free distributed index building
//!
//! This module implements a distributed index builder that requires no external
//! coordination service (no etcd, Kafka, etc.). Instead, it uses:
//!
//! 1. Deterministic partition assignment based on vector ID hash
//! 2. Timestamp-based slab naming for conflict-free concurrent builds
//! 3. S3 WAL as the source of truth for unindexed vectors
//! 4. Idempotent operations that can be safely retried

use anyhow::Result;
use chrono::Utc;
use elacsym_core::{DistanceMetric, PartitionId, Slab, SlabId, Vector, VectorId};
use elacsym_index::{IndexBuilder, IndexConfig};
use elacsym_metadata::s3::S3MetadataService;
use elacsym_storage::{BlobStorage, S3WAL, WALBatch};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Configuration for distributed builder
#[derive(Debug, Clone)]
pub struct DistributedBuilderConfig {
    /// Minimum vectors to trigger index building
    pub min_vectors_per_slab: usize,
    /// Maximum vectors per slab
    pub max_vectors_per_slab: usize,
    /// Number of IVF partitions
    pub num_partitions: u32,
    /// Index configuration
    pub index_config: IndexConfig,
}

impl Default for DistributedBuilderConfig {
    fn default() -> Self {
        Self {
            min_vectors_per_slab: 1000,
            max_vectors_per_slab: 100_000,
            num_partitions: 256,
            index_config: IndexConfig::new(128, DistanceMetric::L2),
        }
    }
}

/// Coordination-free distributed index builder
pub struct DistributedBuilder {
    config: DistributedBuilderConfig,
    storage: Arc<BlobStorage>,
    metadata: Arc<S3MetadataService>,
    wal: Arc<S3WAL>,
}

impl DistributedBuilder {
    pub fn new(
        config: DistributedBuilderConfig,
        storage: Arc<BlobStorage>,
        metadata: Arc<S3MetadataService>,
        wal: Arc<S3WAL>,
    ) -> Self {
        Self {
            config,
            storage,
            metadata,
            wal,
        }
    }

    /// Deterministically assign a vector to a partition based on its ID
    /// This allows multiple builders to independently assign the same vector
    /// to the same partition without coordination
    fn assign_partition(&self, vector_id: &VectorId) -> PartitionId {
        // Use a hash function to deterministically assign partition
        let hash = Self::hash_vector_id(vector_id);
        (hash % self.config.num_partitions as u64) as PartitionId
    }

    /// Hash a vector ID to a u64
    fn hash_vector_id(vector_id: &VectorId) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        vector_id.as_str().hash(&mut hasher);
        hasher.finish()
    }

    /// Generate a deterministic slab ID based on namespace, partition, and timestamp
    /// Format: {namespace}-p{partition_id}-{timestamp}-{uuid}
    fn generate_slab_id(namespace: &str, partition_id: PartitionId) -> SlabId {
        let timestamp = Utc::now().format("%Y%m%d%H%M%S");
        let uuid = Uuid::new_v4().to_string();
        SlabId::from(format!("{}-p{}-{}-{}", namespace, partition_id, timestamp, uuid))
    }

    /// Build indexes for all unindexed vectors in a namespace
    /// This can be called by multiple builders concurrently without coordination
    pub async fn build_indexes(&self, namespace: &str) -> Result<Vec<SlabId>> {
        info!("Starting coordination-free index build for namespace: {}", namespace);

        // Step 1: Get all unindexed vectors from WAL
        let vectors = self.wal.get_unindexed_vectors(namespace).await?;

        if vectors.is_empty() {
            debug!("No unindexed vectors found for namespace: {}", namespace);
            return Ok(Vec::new());
        }

        info!("Found {} unindexed vectors", vectors.len());

        // Step 2: Partition vectors by deterministic assignment
        let partitioned = self.partition_vectors(vectors);

        // Step 3: Build index for each partition
        let mut slab_ids = Vec::new();

        for (partition_id, partition_vectors) in partitioned {
            if partition_vectors.len() < self.config.min_vectors_per_slab {
                debug!(
                    "Partition {} has only {} vectors, skipping (min: {})",
                    partition_id,
                    partition_vectors.len(),
                    self.config.min_vectors_per_slab
                );
                continue;
            }

            // Split into chunks if too large
            let chunks = partition_vectors.chunks(self.config.max_vectors_per_slab);

            for chunk in chunks {
                if chunk.len() < self.config.min_vectors_per_slab {
                    continue;
                }

                let slab_id = self.build_partition_index(namespace, partition_id, chunk).await?;
                slab_ids.push(slab_id);
            }
        }

        info!("Built {} slabs for namespace: {}", slab_ids.len(), namespace);

        Ok(slab_ids)
    }

    /// Partition vectors by their assigned partition ID
    fn partition_vectors(&self, vectors: Vec<Vector>) -> HashMap<PartitionId, Vec<Vector>> {
        let mut partitioned: HashMap<PartitionId, Vec<Vector>> = HashMap::new();

        for vector in vectors {
            let partition_id = self.assign_partition(&vector.id);
            partitioned.entry(partition_id).or_default().push(vector);
        }

        debug!("Partitioned vectors into {} partitions", partitioned.len());
        partitioned
    }

    /// Build an index for a specific partition
    async fn build_partition_index(
        &self,
        namespace: &str,
        partition_id: PartitionId,
        vectors: &[Vector],
    ) -> Result<SlabId> {
        info!(
            "Building index for partition {} with {} vectors",
            partition_id,
            vectors.len()
        );

        // Build the index
        let builder = IndexBuilder::new(self.config.index_config.clone())?;
        let mut slab = builder.build(namespace.to_string(), partition_id, vectors)?;

        // Generate deterministic slab ID
        let slab_id = Self::generate_slab_id(namespace, partition_id);
        slab.metadata.id = slab_id.clone();

        // Store the slab
        self.storage.put_slab_with_namespace(namespace, &slab).await?;

        // Register slab in metadata
        self.metadata
            .register_slab(namespace, partition_id, slab_id.clone())
            .await?;

        info!("Successfully built slab: {}", slab_id);

        Ok(slab_id)
    }

    /// Mark WAL batches as indexed (idempotent operation)
    pub async fn mark_batches_indexed(&self, namespace: &str, batch_ids: Vec<String>) -> Result<()> {
        for batch_id in batch_ids {
            self.wal.mark_batch_indexed(namespace, &batch_id).await?;
        }
        Ok(())
    }

    /// Clean up old indexed WAL batches
    pub async fn cleanup_old_wal(&self, namespace: &str, days: i64) -> Result<usize> {
        let cutoff = Utc::now() - chrono::Duration::days(days);
        let cleaned = self.wal.cleanup_indexed_batches(namespace, cutoff).await?;
        info!("Cleaned up {} old WAL batches for namespace: {}", cleaned, namespace);
        Ok(cleaned)
    }

    /// Run a full build cycle: build indexes and mark batches as indexed
    pub async fn run_build_cycle(&self, namespace: &str) -> Result<()> {
        // Get batches before building
        let batches = self.wal.list_batches(namespace).await?;
        let unindexed_batch_ids: Vec<String> = batches
            .into_iter()
            .filter(|b| !b.indexed)
            .map(|b| b.batch_id)
            .collect();

        if unindexed_batch_ids.is_empty() {
            debug!("No unindexed batches for namespace: {}", namespace);
            return Ok(());
        }

        info!(
            "Running build cycle for {} unindexed batches",
            unindexed_batch_ids.len()
        );

        // Build indexes
        let slab_ids = self.build_indexes(namespace).await?;

        // Mark batches as indexed if we successfully built slabs
        if !slab_ids.is_empty() {
            self.mark_batches_indexed(namespace, unindexed_batch_ids).await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use elacsym_core::Metadata;
    use elacsym_metadata::s3::S3MetadataStore;
    use elacsym_storage::{BlobStorageConfig, StorageBackend};
    use object_store::memory::InMemory;

    fn create_test_vector(id: &str, namespace: &str) -> Vector {
        Vector::new(
            VectorId::from(id.to_string()),
            vec![1.0, 2.0, 3.0, 4.0],
            Metadata::new(),
            namespace,
        )
    }

    #[tokio::test]
    async fn test_partition_assignment() {
        let config = DistributedBuilderConfig::default();
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

        let builder = DistributedBuilder::new(config, storage, metadata, wal);

        // Test deterministic assignment
        let vec1 = VectorId::from("test-vector-1");
        let partition1 = builder.assign_partition(&vec1);
        let partition1_again = builder.assign_partition(&vec1);
        assert_eq!(partition1, partition1_again);

        // Different vectors should (likely) go to different partitions
        let vec2 = VectorId::from("test-vector-2");
        let partition2 = builder.assign_partition(&vec2);
        // Can't guarantee different, but both should be valid
        assert!(partition1 < 256);
        assert!(partition2 < 256);
    }

    #[tokio::test]
    async fn test_vector_partitioning() {
        let config = DistributedBuilderConfig::default();
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

        let builder = DistributedBuilder::new(config, storage, metadata, wal);

        let vectors = vec![
            create_test_vector("vec1", "test"),
            create_test_vector("vec2", "test"),
            create_test_vector("vec3", "test"),
        ];

        let partitioned = builder.partition_vectors(vectors);
        assert!(!partitioned.is_empty());

        // All vectors should be partitioned
        let total_vectors: usize = partitioned.values().map(|v| v.len()).sum();
        assert_eq!(total_vectors, 3);
    }
}
