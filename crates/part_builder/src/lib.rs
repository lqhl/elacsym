#![allow(dead_code)]

//! Utilities for turning ingest batches into queryable parts.

use anyhow::Context;
use chrono::Utc;
use common::{DocId, Error, NamespaceConfig, PartPaths, PartStatistics, Result};
use quant::{encode_rabitq, quantize_int8, Int8Meta, RaBitQMeta};
use rand::{rngs::StdRng, Rng, SeedableRng};
use serde::Serialize;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use tracing::instrument;

/// Result of building a part prior to upload.
#[derive(Debug)]
pub struct PartArtifacts {
    /// Directory containing the persisted artefacts for this part.
    pub output_dir: PathBuf,
    /// Metadata describing the RaBitQ transform for the part.
    pub rabitq_meta: RaBitQMeta,
    /// Packed 1-bit codes for all vectors in the part.
    pub rabitq_codes: Vec<u8>,
    /// Metadata for the int8 quantised representation.
    pub int8_meta: Int8Meta,
    /// Quantised int8 vectors laid out row-major.
    pub int8_vectors: Vec<i8>,
    /// Centroids produced by k-means clustering.
    pub centroids: Vec<Vec<f32>>,
    /// Assignment of each vector to an IVF list.
    pub assignments: Vec<usize>,
    /// Whether the part used the small-part fallback strategy.
    pub small_part_fallback: bool,
    /// Number of IVF centroids trained for this part.
    pub k_trained: usize,
    /// Mean vector norm across the batch.
    pub mean_norm: f32,
    /// Paths where the persisted artefacts were written.
    pub paths: PartPaths,
    /// Summary statistics stored alongside the part.
    pub stats: PartStatistics,
    /// Inclusive document identifier range covered by this part.
    pub doc_id_range: (DocId, DocId),
}

#[derive(Debug, Serialize)]
struct PartMetaFile {
    dim: usize,
    rows: usize,
    k_trained: usize,
    small_part_fallback: bool,
    doc_id_range: (DocId, DocId),
}

fn compute_mean_norm(vectors: &[Vec<f32>]) -> f32 {
    let mut sum = 0.0f32;
    for row in vectors {
        let norm_sq: f32 = row.iter().map(|value| value * value).sum();
        sum += norm_sq.sqrt();
    }
    sum / vectors.len() as f32
}

fn compute_mean_vector(vectors: &[Vec<f32>]) -> Vec<f32> {
    let dim = vectors[0].len();
    let mut centroid = vec![0.0f32; dim];
    for row in vectors {
        for (value, centroid_value) in row.iter().zip(centroid.iter_mut()) {
            *centroid_value += *value;
        }
    }
    let inv_rows = 1.0f32 / vectors.len() as f32;
    for value in centroid.iter_mut() {
        *value *= inv_rows;
    }
    centroid
}

fn clamp_usize(value: usize, min: usize, max: usize) -> usize {
    value.max(min).min(max)
}

fn squared_distance(lhs: &[f32], rhs: &[f32]) -> f32 {
    lhs.iter()
        .zip(rhs.iter())
        .map(|(a, b)| {
            let diff = a - b;
            diff * diff
        })
        .sum()
}

fn initialise_centroids(vectors: &[Vec<f32>], k: usize) -> Vec<Vec<f32>> {
    let n = vectors.len();
    let mut rng = StdRng::seed_from_u64(42);
    let first = rng.gen_range(0..n);
    let mut centroids = Vec::with_capacity(k);
    centroids.push(vectors[first].clone());

    let mut min_distances: Vec<f32> = vectors
        .iter()
        .map(|row| squared_distance(row, &centroids[0]))
        .collect();

    while centroids.len() < k {
        let sum: f32 = min_distances.iter().sum();
        if sum == 0.0 {
            let idx = rng.gen_range(0..n);
            centroids.push(vectors[idx].clone());
        } else {
            let mut target = rng.gen::<f32>() * sum;
            let mut chosen = 0usize;
            for (idx, &dist) in min_distances.iter().enumerate() {
                target -= dist;
                if target <= 0.0 {
                    chosen = idx;
                    break;
                }
            }
            centroids.push(vectors[chosen].clone());
        }

        let last = centroids.last().unwrap().clone();
        for (idx, row) in vectors.iter().enumerate() {
            let dist = squared_distance(row, &last);
            if dist < min_distances[idx] {
                min_distances[idx] = dist;
            }
        }
    }

    centroids
}

fn assign_points(vectors: &[Vec<f32>], centroids: &[Vec<f32>]) -> Vec<usize> {
    let mut assignments = Vec::with_capacity(vectors.len());
    for row in vectors {
        let mut best = 0usize;
        let mut best_dist = squared_distance(row, &centroids[0]);
        for (idx, centroid) in centroids.iter().enumerate().skip(1) {
            let dist = squared_distance(row, centroid);
            if dist < best_dist {
                best = idx;
                best_dist = dist;
            }
        }
        assignments.push(best);
    }
    assignments
}

fn recompute_centroids(vectors: &[Vec<f32>], assignments: &[usize], k: usize) -> Vec<Vec<f32>> {
    let dim = vectors[0].len();
    let mut sums = vec![vec![0.0f32; dim]; k];
    let mut counts = vec![0usize; k];

    for (row, &assignment) in vectors.iter().zip(assignments.iter()) {
        counts[assignment] += 1;
        for (value, acc) in row.iter().zip(sums[assignment].iter_mut()) {
            *acc += *value;
        }
    }

    for (idx, sum) in sums.iter_mut().enumerate() {
        if counts[idx] > 0 {
            let inv = 1.0f32 / counts[idx] as f32;
            for value in sum.iter_mut() {
                *value *= inv;
            }
        } else {
            let replacement = idx % vectors.len();
            *sum = vectors[replacement].clone();
        }
    }

    sums
}

fn train_kmeans(vectors: &[Vec<f32>], k: usize, max_iters: usize) -> (Vec<Vec<f32>>, Vec<usize>) {
    let mut centroids = initialise_centroids(vectors, k);
    let mut assignments = vec![0usize; vectors.len()];

    for _ in 0..max_iters {
        let new_assignments = assign_points(vectors, &centroids);
        if new_assignments == assignments {
            break;
        }
        assignments = new_assignments;
        centroids = recompute_centroids(vectors, &assignments, k);
    }

    (centroids, assignments)
}

fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("failed to create directory {}", path.display()))
        .map_err(Error::Context)
}

fn write_binary(path: &Path, data: &[u8]) -> Result<()> {
    fs::write(path, data)
        .with_context(|| format!("failed to write {}", path.display()))
        .map_err(Error::Context)
}

fn write_rabitq_files(root: &Path, meta: &RaBitQMeta, codes: &[u8]) -> Result<(PathBuf, PathBuf)> {
    let dir = root.join("rabitq");
    ensure_dir(&dir)?;
    let meta_path = dir.join("meta.json");
    let codes_path = dir.join("codes-1bit.bin");

    let meta_bytes = serde_json::to_vec(meta)
        .with_context(|| {
            format!(
                "failed to serialise RaBitQ metadata for {}",
                meta_path.display()
            )
        })
        .map_err(Error::Context)?;
    write_binary(&meta_path, &meta_bytes)?;
    write_binary(&codes_path, codes)?;

    Ok((meta_path, codes_path))
}

fn write_int8_files(root: &Path, meta: &Int8Meta, vectors: &[i8]) -> Result<(PathBuf, PathBuf)> {
    let dir = root.join("vectors").join("int8");
    ensure_dir(&dir)?;
    let scales_path = dir.join("scales.bin");
    let vecpage_path = dir.join("vecpage-00000.bin");

    let mut scales_bytes = Vec::with_capacity(meta.scales.len() * 4);
    for value in &meta.scales {
        scales_bytes.extend_from_slice(&value.to_le_bytes());
    }

    let vector_bytes: Vec<u8> = vectors.iter().map(|value| *value as u8).collect();

    write_binary(&scales_path, &scales_bytes)?;
    write_binary(&vecpage_path, &vector_bytes)?;

    Ok((scales_path, vecpage_path))
}

fn write_fp32_pages(root: &Path, vectors: &[Vec<f32>]) -> Result<PathBuf> {
    let dir = root.join("vectors").join("fp32");
    ensure_dir(&dir)?;
    let vecpage_path = dir.join("vecpage-00000.bin");

    let dim = vectors[0].len();
    let mut bytes = Vec::with_capacity(vectors.len() * dim * 4);
    for row in vectors {
        for value in row {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }

    write_binary(&vecpage_path, &bytes)?;
    Ok(vecpage_path)
}

fn write_centroids(root: &Path, centroids: &[Vec<f32>]) -> Result<PathBuf> {
    let dir = root.join("ivf");
    ensure_dir(&dir)?;
    let centroids_path = dir.join("centroids.bin");

    let dim = centroids.get(0).map(|c| c.len()).unwrap_or(0);
    let mut bytes = Vec::with_capacity(centroids.len() * dim * 4);
    for centroid in centroids {
        for value in centroid {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }

    write_binary(&centroids_path, &bytes)?;
    Ok(centroids_path)
}

fn is_bit_set(buffer: &[u8], bit_index: usize) -> bool {
    let byte_index = bit_index / 8;
    let offset = bit_index % 8;
    if byte_index >= buffer.len() {
        return false;
    }
    (buffer[byte_index] >> offset) & 1 == 1
}

fn write_vbyte(mut value: u64, buf: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value > 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn pack_list_codes(doc_indices: &[usize], dim: usize, global_codes: &[u8]) -> Vec<u8> {
    let total_bits = doc_indices.len() * dim;
    let mut packed = vec![0u8; (total_bits + 7) / 8];

    for (position, &doc_idx) in doc_indices.iter().enumerate() {
        for col in 0..dim {
            let src_bit = doc_idx * dim + col;
            if is_bit_set(global_codes, src_bit) {
                let bit_index = position * dim + col;
                let byte_index = bit_index / 8;
                let offset = (bit_index % 8) as u8;
                packed[byte_index] |= 1 << offset;
            }
        }
    }

    packed
}

fn write_ivf_lists(
    root: &Path,
    assignments: &[usize],
    k: usize,
    dim: usize,
    rabitq_codes: &[u8],
) -> Result<PathBuf> {
    let dir = root.join("ivf").join("lists");
    ensure_dir(&dir)?;

    let mut per_list: Vec<Vec<usize>> = vec![Vec::new(); k];
    for (doc_idx, &list_id) in assignments.iter().enumerate() {
        per_list[list_id].push(doc_idx);
    }

    for (list_id, docs) in per_list.iter().enumerate() {
        let path = dir.join(format!("{list_id:05}.ilist"));
        let mut file = File::create(&path)
            .with_context(|| format!("failed to create IVF list {}", path.display()))
            .map_err(Error::Context)?;

        let mut header = Vec::new();
        header.extend_from_slice(b"ILST");
        header.extend_from_slice(&1u32.to_le_bytes());
        header.extend_from_slice(&(list_id as u32).to_le_bytes());
        header.extend_from_slice(&(docs.len() as u32).to_le_bytes());
        header.extend_from_slice(&(dim as u32).to_le_bytes());
        file.write_all(&header)
            .with_context(|| format!("failed to write IVF header {}", path.display()))
            .map_err(Error::Context)?;

        let mut delta_bytes = Vec::new();
        let mut prev = 0usize;
        for (idx, &doc) in docs.iter().enumerate() {
            let delta = if idx == 0 { doc } else { doc - prev };
            write_vbyte(delta as u64, &mut delta_bytes);
            prev = doc;
        }
        file.write_all(&delta_bytes)
            .with_context(|| format!("failed to write IVF doc deltas {}", path.display()))
            .map_err(Error::Context)?;

        let codes = pack_list_codes(docs, dim, rabitq_codes);
        file.write_all(&codes)
            .with_context(|| format!("failed to write IVF codes {}", path.display()))
            .map_err(Error::Context)?;

        let first = docs.first().copied().unwrap_or(0) as u64;
        let count = docs.len() as u64;
        file.write_all(&first.to_le_bytes())
            .with_context(|| format!("failed to write IVF footer {}", path.display()))
            .map_err(Error::Context)?;
        file.write_all(&count.to_le_bytes())
            .with_context(|| format!("failed to write IVF footer {}", path.display()))
            .map_err(Error::Context)?;
    }

    Ok(dir)
}

fn write_part_metadata(
    root: &Path,
    meta: &PartMetaFile,
    stats: &PartStatistics,
) -> Result<(PathBuf, PathBuf)> {
    let meta_path = root.join("meta.json");
    let stats_path = root.join("stats.json");

    let meta_bytes = serde_json::to_vec(meta)
        .with_context(|| {
            format!(
                "failed to serialise part metadata for {}",
                meta_path.display()
            )
        })
        .map_err(Error::Context)?;
    let stats_bytes = serde_json::to_vec(stats)
        .with_context(|| {
            format!(
                "failed to serialise part statistics for {}",
                stats_path.display()
            )
        })
        .map_err(Error::Context)?;

    write_binary(&meta_path, &meta_bytes)?;
    write_binary(&stats_path, &stats_bytes)?;

    Ok((meta_path, stats_path))
}

fn build_paths(root: &Path) -> PartPaths {
    PartPaths {
        centroids: root
            .join("ivf")
            .join("centroids.bin")
            .to_string_lossy()
            .into(),
        ilist_dir: root.join("ivf").join("lists").to_string_lossy().into(),
        rabitq_meta: root
            .join("rabitq")
            .join("meta.json")
            .to_string_lossy()
            .into(),
        rabitq_codes: root
            .join("rabitq")
            .join("codes-1bit.bin")
            .to_string_lossy()
            .into(),
        vec_int8_dir: root.join("vectors").join("int8").to_string_lossy().into(),
        vec_fp32_dir: root.join("vectors").join("fp32").to_string_lossy().into(),
    }
}

/// Entry point for the ingest pipeline.
#[instrument]
pub async fn build_part(
    cfg: &NamespaceConfig,
    vectors: Vec<Vec<f32>>,
    output_dir: &Path,
) -> Result<PartArtifacts> {
    if vectors.is_empty() {
        return Err(Error::Message(
            "ingest batch must contain vectors".to_string(),
        ));
    }

    let dim = cfg.dim;
    for (idx, row) in vectors.iter().enumerate() {
        if row.len() != dim {
            return Err(Error::Message(format!(
                "vector at index {idx} has dimension {} but namespace expects {dim}",
                row.len()
            )));
        }
    }

    ensure_dir(output_dir)?;

    let rows = vectors.len();
    let small_part_fallback = rows * dim <= 200_000;
    let k_requested = if small_part_fallback {
        1
    } else {
        let approx = (cfg.cluster_factor * (rows as f32).sqrt()).round() as usize;
        let approx = approx.max(1);
        clamp_usize(approx, cfg.k_min.max(1), cfg.k_max.max(1))
    };
    let k_trained = k_requested.min(rows.max(1));

    let (centroids, assignments) = if k_trained == 1 {
        (vec![compute_mean_vector(&vectors)], vec![0usize; rows])
    } else {
        train_kmeans(&vectors, k_trained, 50)
    };

    let (rabitq_meta, rabitq_codes) = encode_rabitq(&vectors).map_err(Error::Context)?;
    let (int8_meta, int8_vectors) = quantize_int8(&vectors).map_err(Error::Context)?;
    let mean_norm = compute_mean_norm(&vectors);

    let (_rabitq_meta_path, _rabitq_codes_path) =
        write_rabitq_files(output_dir, &rabitq_meta, &rabitq_codes)?;
    let (_int8_scales, _int8_vectors_path) =
        write_int8_files(output_dir, &int8_meta, &int8_vectors)?;
    let _fp32_path = write_fp32_pages(output_dir, &vectors)?;
    let _centroids_path = write_centroids(output_dir, &centroids)?;
    let _lists_dir = write_ivf_lists(output_dir, &assignments, k_trained, dim, &rabitq_codes)?;

    let doc_id_range = (0 as DocId, (rows as DocId).saturating_sub(1));
    let created_at = Utc::now().to_rfc3339();
    let stats = PartStatistics {
        created_at,
        mean_norm,
    };
    let meta = PartMetaFile {
        dim,
        rows,
        k_trained,
        small_part_fallback,
        doc_id_range,
    };
    let (_meta_path, _stats_path) = write_part_metadata(output_dir, &meta, &stats)?;

    let paths = build_paths(output_dir);

    Ok(PartArtifacts {
        output_dir: output_dir.to_path_buf(),
        rabitq_meta,
        rabitq_codes,
        int8_meta,
        int8_vectors,
        centroids,
        assignments,
        small_part_fallback,
        k_trained,
        mean_norm,
        paths,
        stats,
        doc_id_range,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::NamespaceConfig;
    use futures::executor::block_on;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct TestMetaFile {
        dim: usize,
        rows: usize,
        k_trained: usize,
        small_part_fallback: bool,
        doc_id_range: (DocId, DocId),
    }

    fn read_meta(path: &Path) -> TestMetaFile {
        let data = fs::read(path.join("meta.json")).expect("meta.json should exist");
        serde_json::from_slice(&data).expect("meta.json should parse")
    }

    #[test]
    fn build_part_small_batch() {
        let cfg = NamespaceConfig::with_dim(3);
        let vectors = vec![
            vec![1.0, 0.0, 2.0],
            vec![0.5, -1.0, 3.0],
            vec![1.5, 1.0, 4.0],
        ];

        let expected_mean = compute_mean_norm(&vectors);
        let tempdir = tempfile::tempdir().expect("tempdir");
        let artifacts = block_on(build_part(&cfg, vectors.clone(), tempdir.path()))
            .expect("build should succeed");

        assert_eq!(artifacts.rabitq_meta.dim, 3);
        assert_eq!(artifacts.rabitq_meta.rows, vectors.len());
        assert!(artifacts.small_part_fallback);
        assert_eq!(artifacts.k_trained, 1);
        assert!((artifacts.mean_norm - expected_mean).abs() < 1e-6);
        assert_eq!(artifacts.assignments, vec![0, 0, 0]);

        let rabitq_meta_path = tempdir.path().join("rabitq").join("meta.json");
        assert!(rabitq_meta_path.exists());
        let rabitq_meta_from_disk: RaBitQMeta =
            serde_json::from_slice(&fs::read(&rabitq_meta_path).expect("read rabitq meta"))
                .expect("parse rabitq meta");
        assert_eq!(rabitq_meta_from_disk.dim, cfg.dim);

        let int8_scales_path = tempdir.path().join("vectors/int8/scales.bin");
        assert!(int8_scales_path.exists());
        assert_eq!(
            fs::metadata(int8_scales_path).unwrap().len(),
            (cfg.dim * 4) as u64
        );

        let fp32_path = tempdir.path().join("vectors/fp32/vecpage-00000.bin");
        assert!(fp32_path.exists());
        assert_eq!(
            fs::metadata(fp32_path).unwrap().len(),
            (vectors.len() * cfg.dim * 4) as u64
        );

        let list_path = tempdir.path().join("ivf/lists/00000.ilist");
        assert!(list_path.exists());

        let meta = read_meta(tempdir.path());
        assert_eq!(meta.rows, vectors.len());
        assert_eq!(meta.doc_id_range, (0, 2));
    }

    #[test]
    fn build_part_large_batch_trains_k() {
        let dim = 64;
        let mut cfg = NamespaceConfig::with_dim(dim);
        cfg.cluster_factor = 1.0;

        let rows = 4000;
        let vectors: Vec<Vec<f32>> = (0..rows)
            .map(|row| (0..dim).map(|col| (row + col) as f32).collect())
            .collect();

        let tempdir = tempfile::tempdir().expect("tempdir");
        let artifacts = block_on(build_part(&cfg, vectors.clone(), tempdir.path()))
            .expect("build should succeed");
        assert!(!artifacts.small_part_fallback);
        assert!(artifacts.k_trained > 1);

        let list_dir = tempdir.path().join("ivf/lists");
        let lists: Vec<_> = fs::read_dir(&list_dir)
            .expect("lists dir")
            .collect::<std::io::Result<Vec<_>>>()
            .expect("collect dir");
        assert_eq!(lists.len(), artifacts.k_trained);

        let centroids_path = tempdir.path().join("ivf/centroids.bin");
        assert_eq!(
            fs::metadata(centroids_path).unwrap().len(),
            (artifacts.k_trained * cfg.dim * 4) as u64
        );

        let meta = read_meta(tempdir.path());
        assert_eq!(meta.k_trained, artifacts.k_trained);
        assert_eq!(meta.small_part_fallback, artifacts.small_part_fallback);
    }

    #[test]
    fn build_part_rejects_dimension_mismatch() {
        let cfg = NamespaceConfig::with_dim(2);
        let vectors = vec![vec![1.0, 2.0], vec![3.0]];

        let tempdir = tempfile::tempdir().expect("tempdir");
        let err = block_on(build_part(&cfg, vectors, tempdir.path()))
            .expect_err("dimension mismatch should fail");
        match err {
            Error::Message(msg) => assert!(msg.contains("dimension")),
            other => panic!("expected Error::Message, got {other:?}"),
        }
    }
}
