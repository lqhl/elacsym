//! Query execution using RaBitQ indexes

use crate::config::SearchConfig;
use elacsym_core::{Error, Result, Slab, ScoredVector, VectorId};
use rabitq::{IvfRabitqIndex, SearchParams};
use tracing::{debug, info};

pub struct QueryExecutor {
    config: SearchConfig,
}

impl QueryExecutor {
    pub fn new(config: SearchConfig) -> Self {
        Self { config }
    }

    /// Search a single slab
    pub fn search_slab(&self, slab: &Slab, query: &[f32]) -> Result<Vec<ScoredVector>> {
        // Verify checksum
        if !slab.verify_checksum() {
            return Err(Error::Index(
                "Slab checksum verification failed".to_string(),
            ));
        }

        // Deserialize RaBitQ index
        let index: IvfRabitqIndex = bincode::deserialize(&slab.index_data)
            .map_err(|e| Error::Serialization(format!("Index deserialization failed: {}", e)))?;

        debug!(
            "Searching slab {} with {} vectors",
            slab.metadata.id,
            slab.vector_count()
        );

        // Create search parameters
        let params = SearchParams::new(self.config.top_k, self.config.nprobe);

        // Perform search
        let results = index
            .search(query, params)
            .map_err(|e| Error::Index(format!("RaBitQ search failed: {}", e)))?;

        // Convert results to ScoredVector
        let mut scored_vectors = Vec::new();
        for (idx, score) in results {
            if idx < slab.vector_ids.len() {
                let vector_id = slab.vector_ids[idx].clone();
                scored_vectors.push(ScoredVector::new(vector_id, score, None));
            }
        }

        info!(
            "Search completed: {} results from slab {}",
            scored_vectors.len(),
            slab.metadata.id
        );

        Ok(scored_vectors)
    }

    /// Merge results from multiple slabs
    pub fn merge_results(&self, mut all_results: Vec<Vec<ScoredVector>>) -> Vec<ScoredVector> {
        // Flatten all results
        let mut merged: Vec<ScoredVector> = all_results
            .iter_mut()
            .flat_map(|v| v.drain(..))
            .collect();

        // Sort by score (descending)
        merged.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

        // Take top-k
        merged.truncate(self.config.top_k);

        merged
    }

    /// Search multiple slabs and merge results
    pub fn search_slabs(&self, slabs: &[Slab], query: &[f32]) -> Result<Vec<ScoredVector>> {
        let mut all_results = Vec::new();

        for slab in slabs {
            match self.search_slab(slab, query) {
                Ok(results) => all_results.push(results),
                Err(e) => {
                    // Log error but continue searching other slabs
                    tracing::warn!("Failed to search slab {}: {}", slab.metadata.id, e);
                }
            }
        }

        if all_results.is_empty() {
            return Ok(Vec::new());
        }

        Ok(self.merge_results(all_results))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merge_results() {
        let config = SearchConfig::default();
        let executor = QueryExecutor::new(config);

        let results1 = vec![
            ScoredVector::new(VectorId::from("v1"), 0.9, None),
            ScoredVector::new(VectorId::from("v2"), 0.7, None),
        ];

        let results2 = vec![
            ScoredVector::new(VectorId::from("v3"), 0.95, None),
            ScoredVector::new(VectorId::from("v4"), 0.6, None),
        ];

        let merged = executor.merge_results(vec![results1, results2]);

        assert_eq!(merged.len(), 4);
        assert_eq!(merged[0].id.as_str(), "v3"); // Highest score
        assert_eq!(merged[1].id.as_str(), "v1");
    }
}
