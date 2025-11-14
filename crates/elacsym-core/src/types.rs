//! Common types used throughout elacsym

use serde::{Deserialize, Serialize};

/// Distance metric for vector similarity
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DistanceMetric {
    /// Euclidean distance (L2)
    L2,
    /// Inner product
    InnerProduct,
    /// Cosine similarity
    Cosine,
}

/// Vector dimension
pub type Dimension = usize;

/// Top-k results count
pub type TopK = usize;

/// Number of probes for IVF search
pub type NumProbes = usize;

/// Partition ID
pub type PartitionId = u32;

/// Sequence number for ordering operations
pub type SequenceNumber = u64;
