//! Partition management for geometric partitioning

use elacsym_core::{PartitionId, Result, Error};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Information about a partition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionInfo {
    pub id: PartitionId,
    pub centroid: Vec<f32>,
    pub vector_count: usize,
    pub slab_ids: Vec<String>,
}

/// Manages partitions and centroid assignment
pub struct PartitionManager {
    dimension: usize,
    partitions: HashMap<PartitionId, PartitionInfo>,
}

impl PartitionManager {
    pub fn new(dimension: usize) -> Self {
        Self {
            dimension,
            partitions: HashMap::new(),
        }
    }

    /// Add a partition with its centroid
    pub fn add_partition(&mut self, id: PartitionId, centroid: Vec<f32>) -> Result<()> {
        if centroid.len() != self.dimension {
            return Err(Error::DimensionMismatch {
                expected: self.dimension,
                actual: centroid.len(),
            });
        }

        let info = PartitionInfo {
            id,
            centroid,
            vector_count: 0,
            slab_ids: Vec::new(),
        };

        self.partitions.insert(id, info);
        Ok(())
    }

    /// Find the nearest partition for a vector
    pub fn assign_partition(&self, vector: &[f32]) -> Result<PartitionId> {
        if vector.len() != self.dimension {
            return Err(Error::DimensionMismatch {
                expected: self.dimension,
                actual: vector.len(),
            });
        }

        if self.partitions.is_empty() {
            return Err(Error::Index("No partitions available".to_string()));
        }

        // Find nearest centroid using L2 distance
        let mut min_distance = f32::MAX;
        let mut nearest_id = 0;

        for (id, info) in &self.partitions {
            let distance = self.l2_distance(vector, &info.centroid);
            if distance < min_distance {
                min_distance = distance;
                nearest_id = *id;
            }
        }

        Ok(nearest_id)
    }

    /// Find top-k nearest partitions (for nprobe)
    pub fn find_nearest_partitions(&self, vector: &[f32], k: usize) -> Result<Vec<PartitionId>> {
        if vector.len() != self.dimension {
            return Err(Error::DimensionMismatch {
                expected: self.dimension,
                actual: vector.len(),
            });
        }

        let mut distances: Vec<(PartitionId, f32)> = self
            .partitions
            .iter()
            .map(|(id, info)| (*id, self.l2_distance(vector, &info.centroid)))
            .collect();

        distances.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        Ok(distances.into_iter().take(k).map(|(id, _)| id).collect())
    }

    /// Update partition statistics
    pub fn update_partition_stats(&mut self, id: PartitionId, vector_count: usize, slab_id: String) {
        if let Some(info) = self.partitions.get_mut(&id) {
            info.vector_count = vector_count;
            if !info.slab_ids.contains(&slab_id) {
                info.slab_ids.push(slab_id);
            }
        }
    }

    /// Get partition info
    pub fn get_partition(&self, id: PartitionId) -> Option<&PartitionInfo> {
        self.partitions.get(&id)
    }

    /// Get all partitions
    pub fn get_all_partitions(&self) -> &HashMap<PartitionId, PartitionInfo> {
        &self.partitions
    }

    /// Calculate L2 distance between two vectors
    fn l2_distance(&self, a: &[f32], b: &[f32]) -> f32 {
        a.iter()
            .zip(b.iter())
            .map(|(x, y)| (x - y).powi(2))
            .sum::<f32>()
            .sqrt()
    }

    /// Check if a partition should be split (too many vectors)
    pub fn should_split(&self, id: PartitionId, threshold: usize) -> bool {
        self.partitions
            .get(&id)
            .map(|info| info.vector_count > threshold)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_partition_assignment() {
        let mut manager = PartitionManager::new(3);

        manager.add_partition(0, vec![1.0, 0.0, 0.0]).unwrap();
        manager.add_partition(1, vec![0.0, 1.0, 0.0]).unwrap();
        manager.add_partition(2, vec![0.0, 0.0, 1.0]).unwrap();

        let vector = vec![0.9, 0.1, 0.1];
        let assigned = manager.assign_partition(&vector).unwrap();
        assert_eq!(assigned, 0); // Nearest to [1,0,0]
    }

    #[test]
    fn test_find_nearest_partitions() {
        let mut manager = PartitionManager::new(2);

        manager.add_partition(0, vec![0.0, 0.0]).unwrap();
        manager.add_partition(1, vec![1.0, 0.0]).unwrap();
        manager.add_partition(2, vec![0.0, 1.0]).unwrap();

        let vector = vec![0.5, 0.5];
        let nearest = manager.find_nearest_partitions(&vector, 2).unwrap();
        assert_eq!(nearest.len(), 2);
    }
}
