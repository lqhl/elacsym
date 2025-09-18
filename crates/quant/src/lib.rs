#![allow(dead_code)]

//! Quantisation kernels used by both the search and build pipelines.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::instrument;

/// Metadata describing how a RaBitQ transform should be applied to a vector.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RaBitQMeta {
    pub dim: usize,
    pub thresholds: Vec<f32>,
}

/// Encodes a batch of floating point vectors into 1-bit RaBitQ codes.
#[instrument]
pub fn encode_rabitq(_vectors: &[Vec<f32>]) -> Result<(RaBitQMeta, Vec<u8>)> {
    anyhow::bail!("RaBitQ encoding is not yet implemented");
}

/// Converts RaBitQ codes back into approximate similarity scores.
pub fn score_with_rabitq(_meta: &RaBitQMeta, _query: &[f32], _codes: &[u8]) -> Vec<f32> {
    Vec::new()
}
