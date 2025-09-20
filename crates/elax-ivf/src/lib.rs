//! Inverted file (IVF) training and assignment primitives.

use std::cmp::Ordering;

use anyhow::{anyhow, ensure, Result};
use rand::{prelude::SliceRandom, rngs::StdRng, Rng, SeedableRng};

/// Distance metric used for IVF centroids and assignment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DistanceMetric {
    /// Cosine distance: `1 - dot(a, b) / (||a|| * ||b||)`.
    Cosine,
    /// Euclidean squared distance.
    EuclideanSquared,
}

impl DistanceMetric {
    fn distance(self, a: &[f32], b: &[f32]) -> Result<f32> {
        ensure!(
            a.len() == b.len(),
            "dimension mismatch: {} vs {}",
            a.len(),
            b.len()
        );
        Ok(match self {
            DistanceMetric::Cosine => cosine_distance(a, b),
            DistanceMetric::EuclideanSquared => euclidean_squared(a, b),
        })
    }
}

/// Training configuration for IVF centroid construction.
#[derive(Clone, Debug)]
pub struct TrainParams {
    /// Number of inverted lists (centroids) to learn.
    pub nlist: usize,
    /// Maximum number of k-means iterations.
    pub max_iterations: usize,
    /// Early stopping tolerance on centroid movement (L2 norm).
    pub tolerance: f32,
    /// Distance metric to optimize under.
    pub metric: DistanceMetric,
    /// RNG seed used for centroid initialization and empty list reseeding.
    pub seed: u64,
}

impl Default for TrainParams {
    fn default() -> Self {
        Self {
            nlist: 1,
            max_iterations: 25,
            tolerance: 1e-4,
            metric: DistanceMetric::EuclideanSquared,
            seed: 42,
        }
    }
}

/// Result of assigning a vector to the IVF structure.
#[derive(Clone, Debug, PartialEq)]
pub struct Assignment {
    /// Index of the chosen inverted list.
    pub list_id: usize,
    /// Distance from the vector to the centroid using the configured metric.
    pub distance: f32,
}

/// Learned IVF model containing the centroid table and metric.
#[derive(Clone, Debug)]
pub struct IvfModel {
    centroids: Vec<Vec<f32>>,
    metric: DistanceMetric,
}

impl IvfModel {
    /// Construct a new model from the provided centroids.
    pub fn new(centroids: Vec<Vec<f32>>, metric: DistanceMetric) -> Result<Self> {
        ensure!(!centroids.is_empty(), "centroid table must not be empty");
        let dim = centroids[0].len();
        ensure!(dim > 0, "centroid dimensionality must be > 0");
        for (idx, centroid) in centroids.iter().enumerate() {
            ensure!(
                centroid.len() == dim,
                "centroid {idx} dimension mismatch: {} vs {}",
                centroid.len(),
                dim
            );
        }
        Ok(Self { centroids, metric })
    }

    /// Return the raw centroids backing the model.
    pub fn centroids(&self) -> &[Vec<f32>] {
        &self.centroids
    }

    /// Assign a vector to its closest centroid.
    pub fn assign(&self, vector: &[f32]) -> Result<Assignment> {
        ensure!(
            !self.centroids.is_empty(),
            "cannot assign without centroids"
        );
        let (list_id, distance) = nearest_centroid(vector, &self.centroids, self.metric)?;
        Ok(Assignment { list_id, distance })
    }

    /// Return the `nprobe` closest centroids sorted by ascending distance.
    pub fn probe_order(&self, vector: &[f32], nprobe: usize) -> Result<Vec<Assignment>> {
        ensure!(nprobe > 0, "nprobe must be > 0");
        ensure!(!self.centroids.is_empty(), "cannot probe without centroids");
        let mut pairs = Vec::with_capacity(self.centroids.len());
        for (idx, centroid) in self.centroids.iter().enumerate() {
            let distance = self.metric.distance(vector, centroid)?;
            pairs.push(Assignment {
                list_id: idx,
                distance,
            });
        }
        pairs.sort_by(|a, b| match a.distance.partial_cmp(&b.distance) {
            Some(Ordering::Equal) | None => a.list_id.cmp(&b.list_id),
            Some(ordering) => ordering,
        });
        pairs.truncate(pairs.len().min(nprobe));
        Ok(pairs)
    }

    /// Distance metric associated with the centroids.
    pub fn metric(&self) -> DistanceMetric {
        self.metric
    }
}

/// Train an IVF model from the provided sample vectors using the given parameters.
pub fn train(samples: &[Vec<f32>], params: TrainParams) -> Result<IvfModel> {
    ensure!(!samples.is_empty(), "training samples must not be empty");
    ensure!(params.nlist > 0, "nlist must be > 0");
    let dim = samples[0].len();
    ensure!(dim > 0, "sample dimensionality must be > 0");
    for (idx, sample) in samples.iter().enumerate() {
        ensure!(
            sample.len() == dim,
            "sample {idx} dimension mismatch: {} vs {}",
            sample.len(),
            dim
        );
    }

    let nlist = params.nlist.min(samples.len());
    let mut rng = StdRng::seed_from_u64(params.seed);
    let mut centroids = initialize_kmeans_pp(samples, nlist, params.metric, &mut rng)?;
    let mut assignments = vec![usize::MAX; samples.len()];
    let max_iterations = params.max_iterations.max(1);
    let tolerance = params.tolerance.max(0.0);

    for _ in 0..max_iterations {
        let changed = assign_samples(samples, &centroids, params.metric, &mut assignments)?;
        let shift = update_centroids(
            samples,
            &assignments,
            &mut centroids,
            params.metric,
            &mut rng,
        )?;
        if !changed || shift <= tolerance {
            break;
        }
    }

    IvfModel::new(centroids, params.metric)
}

/// Simple heuristic to convert a target recall (0-1) into an `nprobe` value.
pub fn nprobe_for_recall(target_recall: f32, nlist: usize) -> usize {
    if nlist == 0 {
        return 0;
    }
    let recall = target_recall.clamp(0.0, 1.0);
    let probes = (recall * nlist as f32).ceil() as usize;
    probes.clamp(1, nlist)
}

fn assign_samples(
    samples: &[Vec<f32>],
    centroids: &[Vec<f32>],
    metric: DistanceMetric,
    assignments: &mut [usize],
) -> Result<bool> {
    let mut changed = false;
    for (idx, sample) in samples.iter().enumerate() {
        let (list_id, _) = nearest_centroid(sample, centroids, metric)?;
        if assignments[idx] != list_id {
            assignments[idx] = list_id;
            changed = true;
        }
    }
    Ok(changed)
}

fn update_centroids(
    samples: &[Vec<f32>],
    assignments: &[usize],
    centroids: &mut [Vec<f32>],
    metric: DistanceMetric,
    rng: &mut StdRng,
) -> Result<f32> {
    let dim = centroids
        .get(0)
        .ok_or_else(|| anyhow!("centroids must not be empty"))?
        .len();
    let mut sums = vec![vec![0.0f32; dim]; centroids.len()];
    let mut counts = vec![0usize; centroids.len()];

    for (sample, &assignment) in samples.iter().zip(assignments.iter()) {
        ensure!(assignment < centroids.len(), "assignment out of bounds");
        for d in 0..dim {
            sums[assignment][d] += sample[d];
        }
        counts[assignment] += 1;
    }

    let mut max_shift = 0.0f32;
    for (idx, centroid) in centroids.iter_mut().enumerate() {
        if counts[idx] == 0 {
            // Dead list – reseed with a random sample to keep it alive.
            let replacement = samples
                .choose(rng)
                .ok_or_else(|| anyhow!("failed to reseed centroid"))?;
            *centroid = replacement.clone();
            if metric == DistanceMetric::Cosine {
                normalize(centroid);
            }
            continue;
        }

        let mut next = sums[idx]
            .iter()
            .map(|value| *value / counts[idx] as f32)
            .collect::<Vec<f32>>();
        if metric == DistanceMetric::Cosine {
            normalize(&mut next);
        }
        let shift = l2_norm_diff(centroid, &next)?;
        if shift > max_shift {
            max_shift = shift;
        }
        *centroid = next;
    }

    Ok(max_shift)
}

fn nearest_centroid(
    vector: &[f32],
    centroids: &[Vec<f32>],
    metric: DistanceMetric,
) -> Result<(usize, f32)> {
    ensure!(!centroids.is_empty(), "centroids must not be empty");
    let mut best_idx = 0usize;
    let mut best_dist = f32::MAX;
    for (idx, centroid) in centroids.iter().enumerate() {
        let distance = metric.distance(vector, centroid)?;
        if distance < best_dist {
            best_dist = distance;
            best_idx = idx;
        }
    }
    Ok((best_idx, best_dist))
}

fn initialize_kmeans_pp(
    samples: &[Vec<f32>],
    nlist: usize,
    metric: DistanceMetric,
    rng: &mut StdRng,
) -> Result<Vec<Vec<f32>>> {
    ensure!(nlist > 0, "nlist must be > 0");
    let mut centroids = Vec::with_capacity(nlist);
    let first = samples
        .choose(rng)
        .ok_or_else(|| anyhow!("no samples available for initialization"))?
        .clone();
    centroids.push(initial_adjust(first, metric));

    while centroids.len() < nlist {
        let mut distances = Vec::with_capacity(samples.len());
        let mut total = 0.0f32;
        for sample in samples {
            let (_, dist) = nearest_centroid(sample, &centroids, metric)?;
            let weight = dist * dist;
            distances.push(weight);
            total += weight;
        }

        let next = if total <= f32::EPSILON {
            samples.choose(rng).unwrap().clone()
        } else {
            let mut cumulative = 0.0f32;
            let target = rng.gen::<f32>() * total;
            let mut chosen = samples.last().unwrap().clone();
            for (sample, weight) in samples.iter().zip(distances.iter()) {
                cumulative += *weight;
                if cumulative >= target {
                    chosen = (*sample).clone();
                    break;
                }
            }
            chosen
        };

        centroids.push(initial_adjust(next, metric));
    }

    Ok(centroids)
}

fn initial_adjust(mut centroid: Vec<f32>, metric: DistanceMetric) -> Vec<f32> {
    if metric == DistanceMetric::Cosine {
        normalize(&mut centroid);
    }
    centroid
}

fn normalize(vector: &mut [f32]) {
    let norm = vector
        .iter()
        .map(|v| *v as f64 * *v as f64)
        .sum::<f64>()
        .sqrt();
    if norm > 0.0 {
        for value in vector.iter_mut() {
            *value /= norm as f32;
        }
    }
}

fn l2_norm_diff(a: &[f32], b: &[f32]) -> Result<f32> {
    ensure!(
        a.len() == b.len(),
        "dimension mismatch: {} vs {}",
        a.len(),
        b.len()
    );
    let mut sum = 0.0f32;
    for i in 0..a.len() {
        let diff = a[i] - b[i];
        sum += diff * diff;
    }
    Ok(sum.sqrt())
}

fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return 1.0;
    }
    1.0 - dot / (norm_a.sqrt() * norm_b.sqrt())
}

fn euclidean_squared(a: &[f32], b: &[f32]) -> f32 {
    let mut sum = 0.0f32;
    for i in 0..a.len() {
        let diff = a[i] - b[i];
        sum += diff * diff;
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clustered_samples() -> Vec<Vec<f32>> {
        let mut samples = Vec::new();
        for i in 0..50 {
            samples.push(vec![i as f32 * 0.02, 0.0]);
        }
        for i in 0..50 {
            samples.push(vec![10.0 + i as f32 * 0.02, 0.0]);
        }
        samples
    }

    #[test]
    fn kmeans_separates_clusters() {
        let samples = clustered_samples();
        let params = TrainParams {
            nlist: 2,
            max_iterations: 50,
            tolerance: 1e-4,
            metric: DistanceMetric::EuclideanSquared,
            seed: 7,
        };
        let model = train(&samples, params).expect("train");
        assert_eq!(model.centroids().len(), 2);

        let left = model.assign(&[0.1, 0.0]).expect("assign left").list_id;
        let right = model.assign(&[10.1, 0.0]).expect("assign right").list_id;
        assert_ne!(
            left, right,
            "points from different clusters map to same list"
        );
    }

    #[test]
    fn probe_order_returns_sorted_centroids() {
        let samples = clustered_samples();
        let params = TrainParams {
            nlist: 3,
            max_iterations: 30,
            tolerance: 1e-4,
            metric: DistanceMetric::EuclideanSquared,
            seed: 11,
        };
        let model = train(&samples, params).expect("train");
        let probes = model.probe_order(&[0.0, 0.0], 2).expect("probe");
        assert_eq!(probes.len(), 2);
        assert!(probes[0].distance <= probes[1].distance);
    }

    #[test]
    fn nprobe_heuristic_clamps_bounds() {
        assert_eq!(nprobe_for_recall(0.0, 0), 0);
        assert_eq!(nprobe_for_recall(0.0, 10), 1);
        assert_eq!(nprobe_for_recall(0.5, 10), 5);
        assert_eq!(nprobe_for_recall(1.2, 5), 5);
    }
}
