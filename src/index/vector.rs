//! Vector index using RaBitQ

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::StorageBackend;
use crate::types::{DistanceMetric, DocId, Vector};
use crate::{Error, Result};

/// Vector index wrapper around RaBitQ
///
/// Note: RaBitQ does not support incremental updates. We need to rebuild
/// the entire index when adding new vectors. This wrapper manages:
/// - Original vectors storage
/// - Doc ID mapping (external ID <-> internal index)
/// - Lazy index building
#[derive(Serialize, Deserialize)]
pub struct VectorIndex {
    dimension: usize,
    metric: DistanceMetric,

    /// Map from DocId to internal vector index
    id_map: HashMap<DocId, usize>,

    /// Reverse map from internal index to DocId
    pub reverse_map: Vec<DocId>,

    /// All vectors stored (needed for index rebuilding)
    pub vectors: Vec<Vector>,

    /// Whether index needs rebuilding
    #[serde(skip)]
    needs_rebuild: bool,

    /// The actual RaBitQ index (not serialized, rebuilt on load)
    #[serde(skip)]
    index: Option<rabitq::RaBitQ>,
}

impl VectorIndex {
    pub fn new(dimension: usize, metric: DistanceMetric) -> Result<Self> {
        if metric != DistanceMetric::L2 {
            return Err(Error::internal(
                "RaBitQ currently only supports L2 distance metric",
            ));
        }

        Ok(Self {
            dimension,
            metric,
            id_map: HashMap::new(),
            reverse_map: Vec::new(),
            vectors: Vec::new(),
            needs_rebuild: false,
            index: None,
        })
    }

    /// Add vectors to the index (marks for rebuild)
    pub fn add(&mut self, ids: &[DocId], vectors: &[Vector]) -> Result<()> {
        if ids.len() != vectors.len() {
            return Err(Error::internal("IDs and vectors length mismatch"));
        }

        for (doc_id, vector) in ids.iter().zip(vectors.iter()) {
            if vector.len() != self.dimension {
                return Err(Error::InvalidSchema(format!(
                    "Vector dimension mismatch: expected {}, got {}",
                    self.dimension,
                    vector.len()
                )));
            }

            // Check if ID already exists (update case)
            if let Some(&internal_id) = self.id_map.get(doc_id) {
                // Update existing vector
                self.vectors[internal_id] = vector.clone();
            } else {
                // Add new vector
                let internal_id = self.vectors.len();
                self.id_map.insert(*doc_id, internal_id);
                self.reverse_map.push(*doc_id);
                self.vectors.push(vector.clone());
            }
        }

        self.needs_rebuild = true;
        Ok(())
    }

    /// Build or rebuild the RaBitQ index
    pub fn build_index(&mut self) -> Result<()> {
        if self.vectors.is_empty() {
            return Ok(());
        }

        // RaBitQ requires dimension to be multiple of 64
        let padded_dim = self.dimension.div_ceil(64) * 64;

        // Create temporary directory for RaBitQ files
        let temp_dir =
            std::env::temp_dir().join(format!("elacsym_rabitq_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir)
            .map_err(|e| Error::internal(format!("Failed to create temp dir: {}", e)))?;

        let base_path = temp_dir.join("base.fvecs");
        let centroid_path = temp_dir.join("centroids.fvecs");

        // Write vectors to fvecs format
        self.write_fvecs(&base_path, &self.vectors, padded_dim)?;

        // Generate centroids using simple k-means
        let k = (self.vectors.len() as f32).sqrt() as usize;
        let k = k.clamp(1, 256); // Limit centroids between 1 and 256
        let centroids = self.generate_centroids(k, padded_dim)?;
        self.write_fvecs(&centroid_path, &centroids, padded_dim)?;

        // Build RaBitQ index
        let rabitq_index = rabitq::RaBitQ::from_path(&base_path, &centroid_path);

        self.index = Some(rabitq_index);
        self.needs_rebuild = false;

        // Clean up temporary files
        let _ = std::fs::remove_dir_all(&temp_dir);

        Ok(())
    }

    /// Get the number of vectors in the index
    pub fn vector_count(&self) -> usize {
        self.vectors.len()
    }

    /// Search for nearest neighbors
    pub fn search(&mut self, query: &Vector, top_k: usize) -> Result<Vec<(DocId, f32)>> {
        if query.len() != self.dimension {
            return Err(Error::InvalidQuery(format!(
                "Query vector dimension mismatch: expected {}, got {}",
                self.dimension,
                query.len()
            )));
        }

        // Rebuild index if needed
        if self.needs_rebuild || self.index.is_none() {
            self.build_index()?;
        }

        let index = self
            .index
            .as_ref()
            .ok_or_else(|| Error::internal("Index not built"))?;

        // Pad query vector
        let padded_dim = self.dimension.div_ceil(64) * 64;
        let mut padded_query = query.clone();
        padded_query.resize(padded_dim, 0.0);

        // Query parameters
        let probe = (self.vectors.len() as f32).sqrt() as usize;
        let probe = probe.clamp(1, 256);

        // RaBitQ search returns Vec<(distance, internal_id)>
        let results = index.query(&padded_query, probe, top_k * 2, true);

        // Map internal IDs back to DocIds
        let mapped_results: Vec<(DocId, f32)> = results
            .into_iter()
            .filter_map(|(dist, internal_id)| {
                self.reverse_map
                    .get(internal_id as usize)
                    .map(|&doc_id| (doc_id, dist))
            })
            .take(top_k)
            .collect();

        Ok(mapped_results)
    }

    /// Serialize index to bytes
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self)
            .map_err(|e| Error::internal(format!("Failed to serialize index: {}", e)))
    }

    /// Deserialize index from bytes
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        let mut index: Self = serde_json::from_slice(data)
            .map_err(|e| Error::internal(format!("Failed to deserialize index: {}", e)))?;

        // Mark for rebuild since we don't serialize the RaBitQ index
        index.needs_rebuild = true;

        Ok(index)
    }

    /// Build and persist segment-level index to storage
    ///
    /// This is the key method for per-segment indexing:
    /// 1. Builds RaBitQ index from vectors in this segment
    /// 2. Serializes index metadata (vectors, mappings, centroids)
    /// 3. Uploads to storage at {namespace}/segments/{segment_id}.rabitq
    pub async fn build_and_persist(
        &mut self,
        storage: Arc<dyn StorageBackend>,
        segment_id: &str,
        namespace: &str,
    ) -> Result<String> {
        if self.vectors.is_empty() {
            return Err(Error::internal("Cannot persist empty index"));
        }

        // 1. Build RaBitQ index (ensures index is up-to-date)
        self.build_index()?;

        // 2. Serialize to JSON (includes vectors, id_map, reverse_map)
        // Note: RaBitQ index itself is not serialized, will be rebuilt on load
        let index_bytes = self.to_bytes()?;

        // 3. Generate storage path
        let index_path = format!("{}/segments/{}.rabitq", namespace, segment_id);

        // 4. Upload to storage
        tracing::info!(
            "Persisting vector index to {} ({} vectors, {} bytes)",
            index_path,
            self.vectors.len(),
            index_bytes.len()
        );
        storage.put(&index_path, Bytes::from(index_bytes)).await?;

        Ok(index_path)
    }

    /// Load segment index from storage
    ///
    /// Downloads and deserializes a per-segment vector index.
    /// The RaBitQ index will be rebuilt on first search.
    pub async fn load_from_storage(
        storage: Arc<dyn StorageBackend>,
        index_path: &str,
    ) -> Result<Self> {
        tracing::info!("Loading vector index from {}", index_path);

        let data = storage.get(index_path).await?;
        let mut index = Self::from_bytes(&data)?;

        // Rebuild RaBitQ index immediately to avoid search-time overhead
        index.build_index()?;

        Ok(index)
    }

    /// Write vectors to fvecs format (used by RaBitQ)
    fn write_fvecs(&self, path: &std::path::Path, vectors: &[Vector], dim: usize) -> Result<()> {
        use std::io::Write;

        let mut file = std::fs::File::create(path)
            .map_err(|e| Error::internal(format!("Failed to create fvecs file: {}", e)))?;

        for vector in vectors {
            // Write dimension as u32
            let dim_bytes = (dim as u32).to_le_bytes();
            file.write_all(&dim_bytes)
                .map_err(|e| Error::internal(format!("Failed to write dimension: {}", e)))?;

            // Write vector values (padded to dim)
            for i in 0..dim {
                let value = if i < vector.len() { vector[i] } else { 0.0 };
                let value_bytes = value.to_le_bytes();
                file.write_all(&value_bytes)
                    .map_err(|e| Error::internal(format!("Failed to write vector value: {}", e)))?;
            }
        }

        Ok(())
    }

    /// Generate centroids using simple k-means
    fn generate_centroids(&self, k: usize, dim: usize) -> Result<Vec<Vector>> {
        if self.vectors.is_empty() {
            return Ok(vec![]);
        }

        let k = k.min(self.vectors.len());

        // Simple k-means++ initialization
        let mut centroids = Vec::with_capacity(k);
        let mut rng = fastrand::Rng::new();

        // First centroid: random vector
        let first_idx = rng.usize(..self.vectors.len());
        let mut first = self.vectors[first_idx].clone();
        first.resize(dim, 0.0);
        centroids.push(first);

        // Remaining centroids: k-means++ style
        for _ in 1..k {
            let mut max_dist = 0.0;
            let mut farthest_idx = 0;

            for (idx, vec) in self.vectors.iter().enumerate() {
                let mut min_dist = f32::MAX;
                for centroid in &centroids {
                    let dist = self.l2_distance(vec, centroid);
                    min_dist = min_dist.min(dist);
                }

                if min_dist > max_dist {
                    max_dist = min_dist;
                    farthest_idx = idx;
                }
            }

            let mut new_centroid = self.vectors[farthest_idx].clone();
            new_centroid.resize(dim, 0.0);
            centroids.push(new_centroid);
        }

        Ok(centroids)
    }

    /// Calculate L2 distance between two vectors
    fn l2_distance(&self, a: &[f32], b: &[f32]) -> f32 {
        a.iter()
            .zip(b.iter())
            .map(|(x, y)| (x - y).powi(2))
            .sum::<f32>()
            .sqrt()
    }

    /// Get number of vectors in index
    pub fn len(&self) -> usize {
        self.vectors.len()
    }

    /// Check if index is empty
    pub fn is_empty(&self) -> bool {
        self.vectors.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vector_index_basic() {
        let mut index = VectorIndex::new(128, DistanceMetric::L2).unwrap();

        // Add some vectors
        let ids = vec![1, 2, 3, 4, 5];
        let vectors = vec![
            vec![1.0; 128],
            vec![2.0; 128],
            vec![3.0; 128],
            vec![4.0; 128],
            vec![5.0; 128],
        ];

        index.add(&ids, &vectors).unwrap();
        assert_eq!(index.len(), 5);
        assert!(index.needs_rebuild);

        // Search (will trigger index build)
        let query = vec![2.5; 128];
        let results = index.search(&query, 2).unwrap();

        assert_eq!(results.len(), 2);
        // Should return closest vectors (IDs 2 and 3)
        assert!(results.iter().any(|(id, _)| *id == 2 || *id == 3));
    }

    #[test]
    fn test_vector_index_serialization() {
        let mut index = VectorIndex::new(64, DistanceMetric::L2).unwrap();

        let ids = vec![1, 2, 3];
        let vectors = vec![vec![1.0; 64], vec![2.0; 64], vec![3.0; 64]];

        index.add(&ids, &vectors).unwrap();

        // Serialize
        let bytes = index.to_bytes().unwrap();

        // Deserialize
        let mut loaded_index = VectorIndex::from_bytes(&bytes).unwrap();
        assert_eq!(loaded_index.len(), 3);
        assert!(loaded_index.needs_rebuild);

        // Search should work after deserialization
        let query = vec![1.5; 64];
        let results = loaded_index.search(&query, 2).unwrap();
        assert_eq!(results.len(), 2);
    }
}
