//! Index builder using RaBitQ

use crate::config::IndexConfig;
use elacsym_core::{Error, Result, Slab, Vector, VectorId};
use rabitq::{IvfRabitqIndex, DistanceMetric as RabitqMetric};
use tracing::{info, debug};

pub struct IndexBuilder {
    config: IndexConfig,
}

impl IndexBuilder {
    pub fn new(config: IndexConfig) -> Result<Self> {
        config.validate().map_err(|e| Error::Configuration(e))?;
        Ok(Self { config })
    }

    /// Build a RaBitQ index from vectors
    pub fn build(
        &self,
        namespace: String,
        partition_id: u32,
        vectors: &[Vector],
    ) -> Result<Slab> {
        if vectors.is_empty() {
            return Err(Error::Index("Cannot build index from empty vector set".to_string()));
        }

        // Validate dimensions
        let expected_dim = self.config.dimension;
        for (i, v) in vectors.iter().enumerate() {
            if v.values.len() != expected_dim {
                return Err(Error::DimensionMismatch {
                    expected: expected_dim,
                    actual: v.values.len(),
                });
            }
        }

        info!(
            "Building RaBitQ index for partition {} with {} vectors",
            partition_id,
            vectors.len()
        );

        // Extract vector data
        let data: Vec<Vec<f32>> = vectors.iter().map(|v| v.values.clone()).collect();
        let vector_ids: Vec<VectorId> = vectors.iter().map(|v| v.id.clone()).collect();

        // Convert metric
        let metric = match self.config.metric {
            elacsym_core::DistanceMetric::L2 => RabitqMetric::L2,
            elacsym_core::DistanceMetric::InnerProduct => RabitqMetric::InnerProduct,
            elacsym_core::DistanceMetric::Cosine => {
                // RaBitQ doesn't have cosine, use inner product (vectors should be normalized)
                debug!("Using InnerProduct for Cosine metric (ensure vectors are normalized)");
                RabitqMetric::InnerProduct
            }
        };

        // Build index using RaBitQ
        let index = IvfRabitqIndex::train(
            &data,
            self.config.nlist,
            self.config.total_bits as u32,
            metric,
            rabitq::Rotator::Hadamard, // Use Fast Hadamard Transform
            self.config.seed.unwrap_or(42) as i64,
            self.config.use_faster_config,
        )
        .map_err(|e| Error::Index(format!("RaBitQ training failed: {}", e)))?;

        // Serialize index
        let index_data = bincode::serialize(&index)
            .map_err(|e| Error::Serialization(format!("Index serialization failed: {}", e)))?;

        info!(
            "Index built successfully: {} bytes, {} vectors",
            index_data.len(),
            vectors.len()
        );

        // Create slab
        Ok(Slab::new(
            namespace,
            partition_id,
            self.config.dimension,
            index_data,
            vector_ids,
        ))
    }

    /// Build index with pre-computed clusters (from external source like FAISS)
    pub fn build_with_clusters(
        &self,
        namespace: String,
        partition_id: u32,
        vectors: &[Vector],
        centroids: &[Vec<f32>],
        assignments: &[usize],
    ) -> Result<Slab> {
        if vectors.is_empty() {
            return Err(Error::Index("Cannot build index from empty vector set".to_string()));
        }

        if vectors.len() != assignments.len() {
            return Err(Error::Index(
                "Vector count must match assignments count".to_string(),
            ));
        }

        info!(
            "Building RaBitQ index with {} pre-computed clusters",
            centroids.len()
        );

        let data: Vec<Vec<f32>> = vectors.iter().map(|v| v.values.clone()).collect();
        let vector_ids: Vec<VectorId> = vectors.iter().map(|v| v.id.clone()).collect();

        let metric = match self.config.metric {
            elacsym_core::DistanceMetric::L2 => RabitqMetric::L2,
            elacsym_core::DistanceMetric::InnerProduct => RabitqMetric::InnerProduct,
            elacsym_core::DistanceMetric::Cosine => RabitqMetric::InnerProduct,
        };

        let index = IvfRabitqIndex::train_with_clusters(
            &data,
            centroids,
            assignments,
            self.config.total_bits as u32,
            metric,
            rabitq::Rotator::Hadamard,
            self.config.seed.unwrap_or(42) as i64,
        )
        .map_err(|e| Error::Index(format!("RaBitQ training with clusters failed: {}", e)))?;

        let index_data = bincode::serialize(&index)
            .map_err(|e| Error::Serialization(format!("Index serialization failed: {}", e)))?;

        Ok(Slab::new(
            namespace,
            partition_id,
            self.config.dimension,
            index_data,
            vector_ids,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use elacsym_core::DistanceMetric;

    #[test]
    fn test_index_builder_validation() {
        let config = IndexConfig::new(128, DistanceMetric::L2);
        assert!(IndexBuilder::new(config).is_ok());

        let mut bad_config = IndexConfig::new(128, DistanceMetric::L2);
        bad_config.dimension = 0;
        assert!(IndexBuilder::new(bad_config).is_err());
    }
}
