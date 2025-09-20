//! Extended RaBitQ quantization primitives.

use anyhow::Result;

/// Placeholder quantization routine that echoes the input vector length.
pub fn encode(vector: &[f32]) -> Result<usize> {
    Ok(vector.len())
}
