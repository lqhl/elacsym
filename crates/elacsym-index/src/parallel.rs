//! Parallel query execution for distributed search
//!
//! This module provides efficient parallel query execution that:
//! 1. Loads multiple slabs concurrently from S3
//! 2. Searches slabs in parallel
//! 3. Merges results efficiently
//! 4. Applies deleted vector filtering from WAL

use anyhow::Result;
use elacsym_core::{Slab, SlabId, ScoredVector, VectorId};
use elacsym_storage::{BlobStorage, S3WAL};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::task::JoinSet;
use tracing::{debug, info};

use crate::{QueryExecutor, SearchConfig};

/// Parallel query executor for distributed search
pub struct ParallelQueryExecutor {
    storage: Arc<BlobStorage>,
    wal: Arc<S3WAL>,
    search_config: SearchConfig,
}

impl ParallelQueryExecutor {
    pub fn new(storage: Arc<BlobStorage>, wal: Arc<S3WAL>, search_config: SearchConfig) -> Self {
        Self {
            storage,
            wal,
            search_config,
        }
    }

    /// Execute a parallel query across all slabs in a namespace
    pub async fn query(
        &self,
        namespace: &str,
        query: &[f32],
    ) -> Result<Vec<ScoredVector>> {
        info!("Starting parallel query for namespace: {}", namespace);

        // Step 1: Get deleted IDs from WAL (to filter out)
        let deleted_ids = self.get_deleted_ids(namespace).await?;
        debug!("Found {} deleted vectors", deleted_ids.len());

        // Step 2: Get unindexed vectors from WAL
        let unindexed_vectors = self.wal.get_unindexed_vectors(namespace).await?;
        debug!("Found {} unindexed vectors", unindexed_vectors.len());

        // Step 3: List all slabs for the namespace
        let slab_ids = self.storage.list_slabs(namespace).await?;
        info!("Querying {} slabs in parallel", slab_ids.len());

        // Step 4: Load and search slabs in parallel
        let main_results = self.search_slabs_parallel(namespace, &slab_ids, query).await?;

        // Step 5: Search unindexed vectors (freshness layer)
        let freshness_results = self.search_unindexed_vectors(&unindexed_vectors, query)?;

        // Step 6: Merge results and filter deleted
        let merged = self.merge_and_filter(
            main_results,
            freshness_results,
            &deleted_ids,
        );

        info!("Parallel query completed, returning {} results", merged.len());

        Ok(merged)
    }

    /// Load and search slabs in parallel
    async fn search_slabs_parallel(
        &self,
        namespace: &str,
        slab_ids: &[SlabId],
        query: &[f32],
    ) -> Result<Vec<ScoredVector>> {
        if slab_ids.is_empty() {
            return Ok(Vec::new());
        }

        let query = query.to_vec();
        let mut join_set = JoinSet::new();

        // Spawn parallel search tasks
        for slab_id in slab_ids {
            let storage = Arc::clone(&self.storage);
            let namespace = namespace.to_string();
            let slab_id = slab_id.clone();
            let query = query.clone();
            let search_config = self.search_config.clone();

            join_set.spawn(async move {
                // Load slab
                let slab = storage
                    .get_slab_with_namespace(&namespace, &slab_id)
                    .await?;

                if let Some(slab) = slab {
                    // Search slab
                    let executor = QueryExecutor::new(search_config);
                    executor.search_slab(&slab, &query)
                } else {
                    Ok(Vec::new())
                }
            });
        }

        // Collect all results
        let mut all_results = Vec::new();

        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(Ok(results)) => all_results.extend(results),
                Ok(Err(e)) => {
                    tracing::warn!("Slab search failed: {}", e);
                    // Continue with other results
                }
                Err(e) => {
                    tracing::warn!("Task join failed: {}", e);
                }
            }
        }

        Ok(all_results)
    }

    /// Search unindexed vectors (in-memory linear search for freshness)
    fn search_unindexed_vectors(
        &self,
        vectors: &[elacsym_core::Vector],
        query: &[f32],
    ) -> Result<Vec<ScoredVector>> {
        if vectors.is_empty() {
            return Ok(Vec::new());
        }

        debug!("Searching {} unindexed vectors", vectors.len());

        // Simple linear search for unindexed vectors
        let mut results: Vec<ScoredVector> = vectors
            .iter()
            .map(|v| {
                let distance = Self::compute_l2_distance(query, &v.values);
                ScoredVector {
                    id: v.id.clone(),
                    score: distance,
                    metadata: v.metadata.clone(),
                }
            })
            .collect();

        // Sort by distance (ascending for L2)
        results.sort_by(|a, b| a.score.partial_cmp(&b.score).unwrap());

        // Take top-k
        results.truncate(self.search_config.top_k);

        Ok(results)
    }

    /// Compute L2 distance between two vectors
    fn compute_l2_distance(a: &[f32], b: &[f32]) -> f32 {
        a.iter()
            .zip(b.iter())
            .map(|(x, y)| (x - y).powi(2))
            .sum::<f32>()
            .sqrt()
    }

    /// Merge main index results and freshness results, filter deleted
    fn merge_and_filter(
        &self,
        mut main_results: Vec<ScoredVector>,
        mut freshness_results: Vec<ScoredVector>,
        deleted_ids: &HashSet<VectorId>,
    ) -> Vec<ScoredVector> {
        // Filter out deleted vectors
        main_results.retain(|v| !deleted_ids.contains(&v.id));
        freshness_results.retain(|v| !deleted_ids.contains(&v.id));

        // Merge results
        let mut merged = Vec::new();
        let mut seen_ids = HashSet::new();

        // First add freshness results (higher priority)
        for result in freshness_results {
            if seen_ids.insert(result.id.clone()) {
                merged.push(result);
            }
        }

        // Then add main results (deduplicate)
        for result in main_results {
            if seen_ids.insert(result.id.clone()) {
                merged.push(result);
            }
        }

        // Sort by score
        merged.sort_by(|a, b| a.score.partial_cmp(&b.score).unwrap());

        // Take top-k
        merged.truncate(self.search_config.top_k);

        merged
    }

    /// Get all deleted vector IDs from WAL
    async fn get_deleted_ids(&self, namespace: &str) -> Result<HashSet<VectorId>> {
        let deleted = self.wal.get_deleted_ids(namespace).await?;
        Ok(deleted.into_iter().collect())
    }
}

/// Batch query execution for multiple queries
pub struct BatchQueryExecutor {
    parallel_executor: ParallelQueryExecutor,
}

impl BatchQueryExecutor {
    pub fn new(parallel_executor: ParallelQueryExecutor) -> Self {
        Self { parallel_executor }
    }

    /// Execute multiple queries in parallel
    pub async fn batch_query(
        &self,
        namespace: &str,
        queries: Vec<Vec<f32>>,
    ) -> Result<Vec<Vec<ScoredVector>>> {
        info!("Executing batch query with {} queries", queries.len());

        let mut join_set = JoinSet::new();

        for query in queries {
            let executor = ParallelQueryExecutor {
                storage: Arc::clone(&self.parallel_executor.storage),
                wal: Arc::clone(&self.parallel_executor.wal),
                search_config: self.parallel_executor.search_config.clone(),
            };
            let namespace = namespace.to_string();

            join_set.spawn(async move {
                executor.query(&namespace, &query).await
            });
        }

        // Collect results in order
        let mut results = Vec::new();

        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(Ok(query_results)) => results.push(query_results),
                Ok(Err(e)) => {
                    tracing::warn!("Query failed: {}", e);
                    results.push(Vec::new());
                }
                Err(e) => {
                    tracing::warn!("Task join failed: {}", e);
                    results.push(Vec::new());
                }
            }
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use elacsym_core::{DistanceMetric, Metadata, Vector};
    use elacsym_metadata::s3::S3MetadataStore;
    use elacsym_storage::{BlobStorageConfig, StorageBackend, WALBatch};
    use object_store::memory::InMemory;

    fn create_test_vector(id: &str, namespace: &str, values: Vec<f32>) -> Vector {
        Vector::new(
            VectorId::from(id.to_string()),
            values,
            Metadata::new(),
            namespace,
        )
    }

    #[tokio::test]
    async fn test_parallel_query() {
        let store = Arc::new(InMemory::new());
        let storage = Arc::new(
            BlobStorage::new(BlobStorageConfig {
                base_path: "test".to_string(),
                backend: StorageBackend::Memory,
            })
            .await
            .unwrap(),
        );

        let wal = Arc::new(S3WAL::new(store, "data".to_string()));

        let search_config = SearchConfig::new(10, 16);
        let executor = ParallelQueryExecutor::new(storage, wal, search_config);

        // Query with empty index
        let query = vec![1.0, 2.0, 3.0];
        let results = executor.query("test_ns", &query).await.unwrap();

        // Should return empty results
        assert_eq!(results.len(), 0);
    }

    #[tokio::test]
    async fn test_merge_and_filter() {
        let store = Arc::new(InMemory::new());
        let storage = Arc::new(
            BlobStorage::new(BlobStorageConfig {
                base_path: "test".to_string(),
                backend: StorageBackend::Memory,
            })
            .await
            .unwrap(),
        );

        let wal = Arc::new(S3WAL::new(store, "data".to_string()));

        let search_config = SearchConfig::new(10, 16);
        let executor = ParallelQueryExecutor::new(storage, wal, search_config);

        let main_results = vec![
            ScoredVector {
                id: VectorId::from("vec1"),
                score: 0.5,
                metadata: Metadata::new(),
            },
            ScoredVector {
                id: VectorId::from("vec2"),
                score: 0.7,
                metadata: Metadata::new(),
            },
        ];

        let freshness_results = vec![
            ScoredVector {
                id: VectorId::from("vec3"),
                score: 0.3,
                metadata: Metadata::new(),
            },
            ScoredVector {
                id: VectorId::from("vec1"),
                score: 0.4, // Duplicate, should use freshness version
                metadata: Metadata::new(),
            },
        ];

        let mut deleted_ids = HashSet::new();
        deleted_ids.insert(VectorId::from("vec2"));

        let merged = executor.merge_and_filter(main_results, freshness_results, &deleted_ids);

        // Should have 2 results (vec3 and vec1, vec2 is deleted)
        assert_eq!(merged.len(), 2);

        // vec3 should be first (lowest score)
        assert_eq!(merged[0].id.as_str(), "vec3");
        assert_eq!(merged[0].score, 0.3);

        // vec1 from freshness should be used
        assert_eq!(merged[1].id.as_str(), "vec1");
        assert_eq!(merged[1].score, 0.4);
    }
}
