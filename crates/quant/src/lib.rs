#![allow(dead_code)]

//! Quantisation kernels used by both the search and build pipelines.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use tracing::instrument;

/// Metadata describing how a RaBitQ transform should be applied to a vector.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RaBitQMeta {
    /// Dimensionality of the original vectors.
    pub dim: usize,
    /// Centroid per dimension used as the reference for residual signs.
    pub centroid: Vec<f32>,
    /// Number of vectors encoded into the codes payload.
    pub rows: usize,
}

impl RaBitQMeta {
    fn is_consistent(&self) -> bool {
        self.dim > 0 && self.dim == self.centroid.len()
    }
}

/// Metadata describing an int8 quantisation transform.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Int8Meta {
    /// Dimensionality of the original vectors.
    pub dim: usize,
    /// Number of vectors quantised into the payload.
    pub rows: usize,
    /// Per-dimension symmetric scaling factors.
    pub scales: Vec<f32>,
}

impl Int8Meta {
    fn is_consistent(&self) -> bool {
        self.dim > 0 && self.rows > 0 && self.scales.len() == self.dim
    }
}

fn pack_bit(codes: &mut [u8], bit_index: usize) {
    let byte_index = bit_index / 8;
    let offset = (bit_index % 8) as u8;
    codes[byte_index] |= 1 << offset;
}

/// Encodes a batch of floating point vectors into 1-bit RaBitQ codes.
#[instrument]
pub fn encode_rabitq(vectors: &[Vec<f32>]) -> Result<(RaBitQMeta, Vec<u8>)> {
    if vectors.is_empty() {
        bail!("RaBitQ encoder requires at least one vector");
    }

    let dim = vectors[0].len();
    if dim == 0 {
        bail!("vector dimensionality must be greater than zero");
    }

    if vectors.iter().any(|row| row.len() != dim) {
        bail!("all vectors must have the same dimensionality");
    }

    let rows = vectors.len();
    let mut centroid = vec![0.0f32; dim];

    for row in vectors {
        for (value, centroid_value) in row.iter().zip(centroid.iter_mut()) {
            *centroid_value += *value;
        }
    }

    let inv_rows = 1.0f32 / rows as f32;
    for value in centroid.iter_mut() {
        *value *= inv_rows;
    }

    let total_bits = rows
        .checked_mul(dim)
        .ok_or_else(|| anyhow!("vector batch too large to encode"))?;
    let mut codes = vec![0u8; (total_bits + 7) / 8];

    for (row_idx, row) in vectors.iter().enumerate() {
        for (col_idx, (&value, &centre)) in row.iter().zip(centroid.iter()).enumerate() {
            if value > centre {
                let bit_index = row_idx * dim + col_idx;
                pack_bit(&mut codes, bit_index);
            }
        }
    }

    let meta = RaBitQMeta {
        dim,
        centroid,
        rows,
    };

    Ok((meta, codes))
}

/// Converts RaBitQ codes back into approximate similarity scores.
pub fn score_with_rabitq(meta: &RaBitQMeta, query: &[f32], codes: &[u8]) -> Vec<f32> {
    if !meta.is_consistent() || meta.rows == 0 {
        return Vec::new();
    }

    if query.len() != meta.dim {
        return Vec::new();
    }

    let Some(total_bits) = meta.dim.checked_mul(meta.rows) else {
        return Vec::new();
    };

    let available_bits = codes.len().saturating_mul(8);
    if available_bits < total_bits {
        return Vec::new();
    }

    let mut query_signs = Vec::with_capacity(meta.dim);
    for (idx, &value) in query.iter().enumerate() {
        let sign = if value > meta.centroid[idx] {
            1.0
        } else {
            -1.0
        };
        query_signs.push(sign);
    }

    let mut scores = vec![0.0f32; meta.rows];

    for row_idx in 0..meta.rows {
        let mut acc = 0.0f32;
        for col_idx in 0..meta.dim {
            let bit_index = row_idx * meta.dim + col_idx;
            let byte_index = bit_index / 8;
            let offset = (bit_index % 8) as u8;
            let bit = (codes[byte_index] >> offset) & 1;
            let code_sign = if bit == 1 { 1.0 } else { -1.0 };
            acc += code_sign * query_signs[col_idx];
        }
        scores[row_idx] = acc;
    }

    scores
}

/// Quantises a batch of vectors into symmetric int8 codes with per-dimension scaling.
#[instrument]
pub fn quantize_int8(vectors: &[Vec<f32>]) -> Result<(Int8Meta, Vec<i8>)> {
    if vectors.is_empty() {
        bail!("int8 quantiser requires at least one vector");
    }

    let dim = vectors[0].len();
    if dim == 0 {
        bail!("vector dimensionality must be greater than zero");
    }

    if vectors.iter().any(|row| row.len() != dim) {
        bail!("all vectors must have the same dimensionality");
    }

    let rows = vectors.len();
    let mut scales = vec![0.0f32; dim];

    for row in vectors {
        for (value, scale) in row.iter().zip(scales.iter_mut()) {
            let abs = value.abs();
            if abs > *scale {
                *scale = abs;
            }
        }
    }

    for scale in scales.iter_mut() {
        if *scale == 0.0 {
            *scale = 1.0;
        }
    }

    let mut codes = Vec::with_capacity(rows * dim);
    for row in vectors {
        for (value, &scale) in row.iter().zip(scales.iter()) {
            let scaled = value / scale * 127.0;
            let quantised = scaled.round().clamp(-127.0, 127.0) as i8;
            codes.push(quantised);
        }
    }

    let meta = Int8Meta { dim, rows, scales };
    Ok((meta, codes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_and_score_round_trip() {
        let vectors = vec![vec![1.0, -1.0], vec![2.0, 2.0], vec![-2.0, -3.0]];

        let (meta, codes) = encode_rabitq(&vectors).expect("encoding should succeed");
        assert_eq!(meta.dim, 2);
        assert_eq!(meta.rows, 3);
        assert_eq!(meta.centroid.len(), 2);

        let query = vec![1.0, 1.0];
        let scores = score_with_rabitq(&meta, &query, &codes);
        assert_eq!(scores, vec![0.0, 2.0, -2.0]);
    }

    #[test]
    fn encode_validates_input() {
        let vectors = vec![vec![1.0, 2.0], vec![3.0]];
        let err = encode_rabitq(&vectors).expect_err("mismatched dims should error");
        assert!(err.to_string().contains("same dimensionality"));
    }

    #[test]
    fn encode_matches_official_sign_rule() {
        let vectors = vec![vec![1.0, 1.0], vec![1.0, 1.0]];

        let (meta, codes) = encode_rabitq(&vectors).expect("encoding should succeed");
        assert_eq!(meta.centroid, vec![1.0, 1.0]);
        assert_eq!(meta.rows, 2);
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0], 0);
    }

    #[test]
    fn quantize_int8_produces_expected_shape() {
        let vectors = vec![vec![1.0, -2.0, 0.5], vec![3.0, 0.0, -1.0]];

        let (meta, codes) = quantize_int8(&vectors).expect("quantisation should succeed");
        assert_eq!(meta.dim, 3);
        assert_eq!(meta.rows, 2);
        assert_eq!(meta.scales.len(), 3);
        assert_eq!(codes.len(), 6);
        assert!(codes.iter().all(|&value| {
            let v = value as i16;
            v >= -127 && v <= 127
        }));
    }

    #[test]
    fn quantize_int8_handles_zero_vectors() {
        let vectors = vec![vec![0.0, 0.0], vec![0.0, 0.0]];

        let (meta, codes) = quantize_int8(&vectors).expect("zero vectors should be quantisable");
        assert_eq!(meta.scales, vec![1.0, 1.0]);
        assert!(codes.iter().all(|&value| value == 0));
    }
}
