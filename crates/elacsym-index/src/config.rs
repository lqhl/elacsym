//! Configuration for indexing and search

use elacsym_core::{DistanceMetric, Dimension, NumProbes, TopK};
use serde::{Deserialize, Serialize};

/// Configuration for building RaBitQ indexes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexConfig {
    /// Vector dimension
    pub dimension: Dimension,

    /// Distance metric
    pub metric: DistanceMetric,

    /// Number of partitions (IVF clusters)
    /// Recommended: sqrt(N) to 4*sqrt(N)
    pub nlist: usize,

    /// Total quantization bits (3-8 recommended)
    /// Higher = better accuracy, more memory
    pub total_bits: usize,

    /// Use faster config for large datasets (>100K vectors)
    /// Speeds up training 100-500x with <1% accuracy loss
    pub use_faster_config: bool,

    /// Random seed for reproducibility
    pub seed: Option<u64>,
}

impl IndexConfig {
    pub fn new(dimension: Dimension, metric: DistanceMetric) -> Self {
        // Default configuration
        Self {
            dimension,
            metric,
            nlist: 1024,
            total_bits: 6,
            use_faster_config: false,
            seed: None,
        }
    }

    /// Auto-configure based on dataset size
    pub fn auto_configure(dimension: Dimension, metric: DistanceMetric, vector_count: usize) -> Self {
        let nlist = if vector_count < 10_000 {
            256
        } else if vector_count < 100_000 {
            1024
        } else if vector_count < 1_000_000 {
            4096
        } else {
            // sqrt(N) for very large datasets
            (vector_count as f64).sqrt() as usize
        };

        let use_faster_config = vector_count > 100_000;

        let total_bits = if vector_count < 100_000 { 6 } else { 5 };

        Self {
            dimension,
            metric,
            nlist,
            total_bits,
            use_faster_config,
            seed: None,
        }
    }

    /// Validate configuration
    pub fn validate(&self) -> Result<(), String> {
        if self.dimension == 0 {
            return Err("Dimension must be greater than 0".to_string());
        }

        if self.nlist == 0 {
            return Err("nlist must be greater than 0".to_string());
        }

        if !(3..=8).contains(&self.total_bits) {
            return Err("total_bits must be between 3 and 8".to_string());
        }

        Ok(())
    }
}

/// Configuration for search queries
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    /// Number of top results to return
    pub top_k: TopK,

    /// Number of partitions to probe
    /// Higher = better recall, slower query
    pub nprobe: NumProbes,

    /// Include vectors in results
    pub include_vectors: bool,

    /// Include metadata in results
    pub include_metadata: bool,
}

impl SearchConfig {
    pub fn new(top_k: TopK, nprobe: NumProbes) -> Self {
        Self {
            top_k,
            nprobe,
            include_vectors: false,
            include_metadata: true,
        }
    }

    /// Auto-configure nprobe based on recall requirements
    pub fn auto_nprobe(nlist: usize, recall_target: f32) -> NumProbes {
        // Heuristic: higher recall needs more probes
        let ratio = if recall_target >= 0.99 {
            0.3
        } else if recall_target >= 0.95 {
            0.2
        } else if recall_target >= 0.90 {
            0.1
        } else {
            0.05
        };

        ((nlist as f32 * ratio) as usize).max(1)
    }
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            top_k: 10,
            nprobe: 16,
            include_vectors: false,
            include_metadata: true,
        }
    }
}
