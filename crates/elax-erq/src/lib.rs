//! Extended RaBitQ quantization primitives.

use anyhow::{ensure, Result};

/// Maximum number of bits supported by the reference encoder.
pub const MAX_BITS: u8 = 16;

/// Training configuration describing the bit budgets for the coarse and rerank paths.
#[derive(Clone, Copy, Debug)]
pub struct TrainConfig {
    pub coarse_bits: u8,
    pub fine_bits: u8,
}

impl Default for TrainConfig {
    fn default() -> Self {
        Self {
            coarse_bits: 1,
            fine_bits: 8,
        }
    }
}

/// Distance metric supported by the ERQ distance estimators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DistanceMetric {
    Cosine,
    EuclideanSquared,
}

/// Learned ERQ model storing per-dimension quantization ranges.
#[derive(Clone, Debug)]
pub struct Model {
    dimension: usize,
    coarse_bits: u8,
    fine_bits: u8,
    mins: Vec<f32>,
    maxs: Vec<f32>,
}

impl Model {
    /// Encode a floating point vector into coarse/fine ERQ codes.
    pub fn encode(&self, vector: &[f32]) -> Result<EncodedVector> {
        ensure!(
            vector.len() == self.dimension,
            "vector dimension mismatch: {} vs {}",
            vector.len(),
            self.dimension
        );
        let fine = self.quantize(vector, self.fine_bits)?;
        let coarse = self.downsample(&fine);
        Ok(EncodedVector { coarse, fine })
    }

    /// Compute the distance between the query and the coarse reconstruction.
    pub fn coarse_distance(
        &self,
        query: &[f32],
        coarse: &[u8],
        metric: DistanceMetric,
    ) -> Result<f32> {
        ensure!(
            coarse.len() == self.dimension,
            "coarse code dimension mismatch: {} vs {}",
            coarse.len(),
            self.dimension
        );
        let approx = self.dequantize(coarse, self.coarse_bits);
        distance(metric, query, &approx)
    }

    /// Compute the distance between the query and the fine reconstruction.
    pub fn fine_distance(
        &self,
        query: &[f32],
        encoded: &EncodedVector,
        metric: DistanceMetric,
    ) -> Result<f32> {
        ensure!(
            encoded.fine.len() == self.dimension,
            "fine code dimension mismatch: {} vs {}",
            encoded.fine.len(),
            self.dimension
        );
        let approx = self.dequantize(&encoded.fine, self.fine_bits);
        distance(metric, query, &approx)
    }

    /// Dimensionality of vectors supported by the model.
    pub fn dimension(&self) -> usize {
        self.dimension
    }

    /// Number of bits allocated to the coarse (x-bit) scan.
    pub fn coarse_bits(&self) -> u8 {
        self.coarse_bits
    }

    /// Number of bits allocated to the rerank (y-bit) path.
    pub fn fine_bits(&self) -> u8 {
        self.fine_bits
    }

    fn quantize(&self, vector: &[f32], bits: u8) -> Result<Vec<u8>> {
        let levels = 1u32 << bits;
        let mut codes = Vec::with_capacity(vector.len());
        for (idx, &value) in vector.iter().enumerate() {
            let min = self.mins[idx];
            let max = self.maxs[idx];
            if max <= min {
                codes.push(0);
                continue;
            }
            let normalized = ((value - min) / (max - min)).clamp(0.0, 1.0);
            let level = (normalized * (levels - 1) as f32).round() as u32;
            codes.push(level.min(levels - 1) as u8);
        }
        Ok(codes)
    }

    fn downsample(&self, fine: &[u8]) -> Vec<u8> {
        let fine_levels = 1u32 << self.fine_bits;
        let coarse_levels = 1u32 << self.coarse_bits;
        fine.iter()
            .map(|&code| {
                let scaled = (code as u32 * coarse_levels) / fine_levels;
                scaled.min(coarse_levels - 1) as u8
            })
            .collect()
    }

    fn dequantize(&self, codes: &[u8], bits: u8) -> Vec<f32> {
        let levels = 1u32 << bits;
        let denom = (levels - 1).max(1) as f32;
        codes
            .iter()
            .enumerate()
            .map(|(idx, &code)| {
                let min = self.mins[idx];
                let max = self.maxs[idx];
                if max <= min {
                    return min;
                }
                min + (code as f32 / denom) * (max - min)
            })
            .collect()
    }
}

/// Train an ERQ model using per-dimension min/max ranges derived from the samples.
pub fn train(samples: &[Vec<f32>], config: TrainConfig) -> Result<Model> {
    ensure!(!samples.is_empty(), "training samples must not be empty");
    ensure!(config.coarse_bits > 0, "coarse bits must be > 0");
    ensure!(config.fine_bits > 0, "fine bits must be > 0");
    ensure!(
        config.coarse_bits <= config.fine_bits,
        "fine bits must be >= coarse bits"
    );
    ensure!(config.coarse_bits <= MAX_BITS, "coarse bits exceed support");
    ensure!(config.fine_bits <= MAX_BITS, "fine bits exceed support");

    let dimension = samples[0].len();
    ensure!(dimension > 0, "sample dimension must be > 0");
    for (idx, sample) in samples.iter().enumerate() {
        ensure!(
            sample.len() == dimension,
            "sample {idx} dimension mismatch: {} vs {}",
            sample.len(),
            dimension
        );
    }

    let mut mins = vec![f32::INFINITY; dimension];
    let mut maxs = vec![f32::NEG_INFINITY; dimension];
    for sample in samples {
        for (idx, &value) in sample.iter().enumerate() {
            if value < mins[idx] {
                mins[idx] = value;
            }
            if value > maxs[idx] {
                maxs[idx] = value;
            }
        }
    }

    for idx in 0..dimension {
        if !mins[idx].is_finite() || !maxs[idx].is_finite() {
            mins[idx] = 0.0;
            maxs[idx] = 0.0;
        }
    }

    Ok(Model {
        dimension,
        coarse_bits: config.coarse_bits,
        fine_bits: config.fine_bits,
        mins,
        maxs,
    })
}

/// Encoded representation storing coarse and fine quantization codes.
#[derive(Clone, Debug)]
pub struct EncodedVector {
    coarse: Vec<u8>,
    fine: Vec<u8>,
}

impl EncodedVector {
    pub fn coarse(&self) -> &[u8] {
        &self.coarse
    }

    pub fn fine(&self) -> &[u8] {
        &self.fine
    }
}

fn distance(metric: DistanceMetric, query: &[f32], approx: &[f32]) -> Result<f32> {
    ensure!(
        query.len() == approx.len(),
        "distance dimension mismatch: {} vs {}",
        query.len(),
        approx.len()
    );
    match metric {
        DistanceMetric::Cosine => cosine_distance(query, approx),
        DistanceMetric::EuclideanSquared => euclidean_squared(query, approx),
    }
}

fn cosine_distance(a: &[f32], b: &[f32]) -> Result<f32> {
    let mut dot = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return Ok(1.0);
    }
    Ok(1.0 - dot / (norm_a.sqrt() * norm_b.sqrt()))
}

fn euclidean_squared(a: &[f32], b: &[f32]) -> Result<f32> {
    let mut sum = 0.0;
    for i in 0..a.len() {
        let diff = a[i] - b[i];
        sum += diff * diff;
    }
    Ok(sum)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn train_and_encode_round_trips_reasonably() {
        let samples = vec![
            vec![1.0, -1.0, 0.5],
            vec![0.5, -0.5, 0.25],
            vec![0.0, 0.25, -0.25],
        ];
        let model = train(&samples, TrainConfig::default()).expect("train");
        let encoded = model.encode(&samples[0]).expect("encode");
        assert_eq!(encoded.coarse().len(), 3);
        assert_eq!(encoded.fine().len(), 3);
        let coarse = model
            .coarse_distance(
                &samples[0],
                encoded.coarse(),
                DistanceMetric::EuclideanSquared,
            )
            .expect("coarse distance");
        let fine = model
            .fine_distance(&samples[0], &encoded, DistanceMetric::EuclideanSquared)
            .expect("fine distance");
        assert!(coarse <= fine + 1e-6);
        assert!(fine <= 1e-4);
    }

    #[test]
    fn respects_bit_configuration() {
        let samples = vec![vec![0.0, 1.0], vec![1.0, 0.0]];
        let model = train(
            &samples,
            TrainConfig {
                coarse_bits: 2,
                fine_bits: 4,
            },
        )
        .expect("train");
        assert_eq!(model.coarse_bits(), 2);
        assert_eq!(model.fine_bits(), 4);
        let encoded = model.encode(&samples[1]).expect("encode");
        assert!(encoded.coarse().iter().all(|&c| c < 4));
        assert!(encoded.fine().iter().all(|&c| c < 16));
    }

    #[test]
    fn distance_handles_constant_dimensions() {
        let samples = vec![vec![1.0, 1.0], vec![1.0, 1.0]];
        let model = train(&samples, TrainConfig::default()).expect("train");
        let encoded = model.encode(&samples[0]).expect("encode");
        let dist = model
            .fine_distance(&[1.0, 1.0], &encoded, DistanceMetric::Cosine)
            .expect("distance");
        assert!(dist.abs() <= 1e-6);
    }

    proptest! {
        #[test]
        fn fine_distance_stays_close_to_training_vectors(
            (dim, samples) in (1usize..6).prop_flat_map(|dim| {
                let sample = prop::collection::vec(-5.0f32..5.0, dim);
                prop::collection::vec(sample, 2..8).prop_map(move |samples| (dim, samples))
            })
        ) {
            let model = train(&samples, TrainConfig::default()).expect("train");
            prop_assert_eq!(model.dimension(), dim);

            let fine_levels = (1u32 << model.fine_bits()) as f32;
            let mut max_error = 0.0f32;
            for idx in 0..model.dimension() {
                let range = model.maxs[idx] - model.mins[idx];
                if range <= 0.0 {
                    continue;
                }
                let step = range / (fine_levels - 1.0);
                max_error += (step * 0.5).powi(2);
            }

            let cosine_slack = 2e-2f32;
            for sample in &samples {
                let norm = sample
                    .iter()
                    .map(|v| v.powi(2))
                    .sum::<f32>()
                    .sqrt();
                if norm <= 1e-2 {
                    continue;
                }
                let encoded = model.encode(sample).expect("encode");
                let fine_l2 = model
                    .fine_distance(sample, &encoded, DistanceMetric::EuclideanSquared)
                    .expect("fine distance");
                prop_assert!(fine_l2 <= max_error + 1e-3);

                let coarse_l2 = model
                    .coarse_distance(sample, encoded.coarse(), DistanceMetric::EuclideanSquared)
                    .expect("coarse distance");
                prop_assert!(coarse_l2 + 1e-6 >= fine_l2);

                let fine_cos = model
                    .fine_distance(sample, &encoded, DistanceMetric::Cosine)
                    .expect("fine cosine");
                let coarse_cos = model
                    .coarse_distance(sample, encoded.coarse(), DistanceMetric::Cosine)
                    .expect("coarse cosine");
                prop_assert!(coarse_cos + cosine_slack >= fine_cos);
                prop_assert!(fine_cos >= -1e-3);
            }
        }
    }
}
