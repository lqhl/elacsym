//! Inverted file (IVF) training and assignment primitives.

use anyhow::Result;

/// Placeholder centroid trainer returning a single centroid.
pub fn train(_samples: &[Vec<f32>]) -> Result<Vec<f32>> {
    Ok(vec![1.0])
}
