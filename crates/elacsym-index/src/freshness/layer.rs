//! Freshness layer implementation

use super::buffer::WriteBuffer;
use crate::{IndexBuilder, IndexConfig, QueryExecutor, SearchConfig};
use elacsym_core::{Error, Result, ScoredVector, Slab, Vector};
use parking_lot::RwLock;
use std::sync::Arc;
use tracing::{debug, info};

/// Freshness layer for providing query freshness
pub struct FreshnessLayer {
    /// Namespace this freshness layer belongs to
    namespace: String,

    /// Index configuration
    index_config: IndexConfig,

    /// Write buffer for recent vectors
    write_buffer: Arc<WriteBuffer>,

    /// Current in-memory index (built from buffer)
    current_index: Arc<RwLock<Option<Slab>>>,

    /// Flush threshold
    flush_threshold: usize,
}

impl FreshnessLayer {
    pub fn new(
        namespace: String,
        index_config: IndexConfig,
        flush_threshold: usize,
    ) -> Self {
        Self {
            namespace,
            index_config,
            write_buffer: Arc::new(WriteBuffer::new(flush_threshold)),
            current_index: Arc::new(RwLock::new(None)),
            flush_threshold,
        }
    }

    /// Add a vector to the freshness layer
    pub async fn add(&self, vector: Vector) -> Result<()> {
        self.write_buffer.add(vector)?;

        // Check if we should rebuild the index
        if self.write_buffer.should_flush() {
            self.rebuild_index().await?;
        }

        Ok(())
    }

    /// Add multiple vectors
    pub async fn add_batch(&self, vectors: Vec<Vector>) -> Result<()> {
        self.write_buffer.add_batch(vectors)?;

        if self.write_buffer.should_flush() {
            self.rebuild_index().await?;
        }

        Ok(())
    }

    /// Rebuild the in-memory index from the buffer
    async fn rebuild_index(&self) -> Result<()> {
        let vectors = self.write_buffer.get_all();

        if vectors.is_empty() {
            return Ok(());
        }

        info!(
            "Rebuilding freshness layer index with {} vectors",
            vectors.len()
        );

        // Build a small RaBitQ index
        // Use a smaller nlist for small datasets
        let mut config = self.index_config.clone();
        config.nlist = (vectors.len() as f64).sqrt().ceil() as usize;
        config.nlist = config.nlist.max(1).min(64); // Limit nlist for small index

        let builder = IndexBuilder::new(config)?;
        let slab = builder.build(self.namespace.clone(), 0, &vectors)?;

        // Update current index
        let mut current_index = self.current_index.write();
        *current_index = Some(slab);

        debug!("Freshness layer index rebuilt successfully");

        Ok(())
    }

    /// Search the freshness layer
    pub async fn search(
        &self,
        query: &[f32],
        search_config: &SearchConfig,
    ) -> Result<Vec<ScoredVector>> {
        // First, rebuild index if needed to include latest writes
        if !self.write_buffer.is_empty() && self.current_index.read().is_none() {
            self.rebuild_index().await?;
        }

        // Search the current index
        let current_index = self.current_index.read();
        if let Some(ref slab) = *current_index {
            let executor = QueryExecutor::new(search_config.clone());
            executor.search_slab(slab, query)
        } else {
            // No index yet, do linear search on buffer
            Ok(self.linear_search(query, search_config.top_k))
        }
    }

    /// Linear search on buffered vectors (fallback)
    fn linear_search(&self, query: &[f32], top_k: usize) -> Vec<ScoredVector> {
        let vectors = self.write_buffer.get_all();

        if vectors.is_empty() {
            return Vec::new();
        }

        let mut results: Vec<(VectorId, f32)> = vectors
            .iter()
            .map(|v| {
                let distance = self.l2_distance(query, &v.values);
                (v.id.clone(), distance)
            })
            .collect();

        // Sort by distance (ascending for L2)
        results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        // Take top-k
        results.truncate(top_k);

        // Convert to ScoredVector
        results
            .into_iter()
            .map(|(id, score)| ScoredVector::new(id, score, None))
            .collect()
    }

    /// Calculate L2 distance
    fn l2_distance(&self, a: &[f32], b: &[f32]) -> f32 {
        a.iter()
            .zip(b.iter())
            .map(|(x, y)| (x - y).powi(2))
            .sum::<f32>()
            .sqrt()
    }

    /// Flush the buffer and return all vectors
    pub fn flush(&self) -> Vec<Vector> {
        // Clear the current index
        let mut current_index = self.current_index.write();
        *current_index = None;

        // Drain the buffer
        self.write_buffer.drain()
    }

    /// Get number of vectors in the freshness layer
    pub fn len(&self) -> usize {
        self.write_buffer.len()
    }

    /// Check if freshness layer is empty
    pub fn is_empty(&self) -> bool {
        self.write_buffer.is_empty()
    }

    /// Get a specific vector by ID
    pub fn get(&self, id: &VectorId) -> Option<Vector> {
        self.write_buffer.get(id)
    }

    /// Remove a vector by ID
    pub fn remove(&self, id: &VectorId) -> Option<Vector> {
        let removed = self.write_buffer.remove(id);

        // Mark index as dirty if we removed something
        if removed.is_some() {
            let mut current_index = self.current_index.write();
            *current_index = None;
        }

        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use elacsym_core::{DistanceMetric, Metadata};

    #[tokio::test]
    async fn test_freshness_layer() {
        let config = IndexConfig::new(128, DistanceMetric::L2);
        let layer = FreshnessLayer::new("test".to_string(), config, 10);

        // Add some vectors
        for i in 0..5 {
            let vector = Vector::new(
                VectorId::from(format!("v{}", i)),
                vec![i as f32; 128],
                Metadata::new(),
                "test",
            );
            layer.add(vector).await.unwrap();
        }

        assert_eq!(layer.len(), 5);

        // Search
        let query = vec![2.5; 128];
        let search_config = SearchConfig::new(3, 4);
        let results = layer.search(&query, &search_config).await.unwrap();

        assert!(!results.is_empty());
    }
}
