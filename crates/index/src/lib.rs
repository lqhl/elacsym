#![allow(dead_code)]

//! Candidate generation over IVF + RaBitQ coded data.

use std::collections::HashSet;
use std::convert::TryFrom;
use std::fs;
use std::path::Path;

use anyhow::Context;
use bitmap::LiveSet;
use common::{Candidate, DocId, Error, ManifestView, NamespaceConfig, PartMetadata, Result};
use quant::{score_with_rabitq, RaBitQMeta};
use rerank::{rerank_fp32, rerank_int8};
use serde::de::DeserializeOwned;
use tracing::instrument;

/// Planner output consumed by the search stage.
#[derive(Debug, Clone, Default)]
pub struct PartSearchPlan {
    pub k: usize,
    pub nprobe: usize,
    pub fallback: bool,
}

/// Compute the number of probes for a part using the documented heuristics.
pub fn plan_for_part(
    cfg: &NamespaceConfig,
    k_trained: usize,
    small_part_fallback: bool,
    probe_fraction: f32,
) -> PartSearchPlan {
    if small_part_fallback {
        return PartSearchPlan {
            k: 1,
            nprobe: 1,
            fallback: true,
        };
    }

    let k = k_trained.max(1);
    let raw = (probe_fraction * k as f32).round() as usize;
    let nprobe = raw.clamp(1, k.min(cfg.nprobe_cap.max(1)));
    PartSearchPlan {
        k,
        nprobe,
        fallback: false,
    }
}

/// Precision to use when reranking stage-1 candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerankPrecision {
    None,
    Int8,
    Fp32,
}

impl RerankPrecision {
    fn from_str(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "int8" => Some(Self::Int8),
            "fp32" => Some(Self::Fp32),
            _ => None,
        }
    }
}

/// Request options accepted by [`search_namespace`].
#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub topk: usize,
    pub probe_fraction: Option<f32>,
    pub rerank_scale: Option<usize>,
    pub rerank_precision: Option<RerankPrecision>,
    pub fp32_rerank_cap: Option<usize>,
}

impl SearchOptions {
    /// Construct a new options struct targeting the provided `topk`.
    pub fn new(topk: usize) -> Self {
        Self {
            topk,
            probe_fraction: None,
            rerank_scale: None,
            rerank_precision: None,
            fp32_rerank_cap: None,
        }
    }
}

/// Executes the two-stage search pipeline for a namespace view.
#[instrument(skip(view, query, opts))]
pub async fn search_namespace(
    view: &ManifestView,
    query: &[f32],
    opts: SearchOptions,
) -> Result<Vec<Candidate>> {
    if opts.topk == 0 {
        return Err(Error::from("topk must be positive"));
    }

    if query.len() != view.namespace.dim {
        return Err(Error::Message(format!(
            "query dimension {} does not match namespace dimension {}",
            query.len(),
            view.namespace.dim
        )));
    }

    let probe_fraction = opts
        .probe_fraction
        .unwrap_or(view.namespace.defaults.probe_fraction)
        .clamp(0.0, 1.0);
    let rerank_scale = opts
        .rerank_scale
        .unwrap_or(view.namespace.defaults.rerank_scale);
    let rerank_precision = opts
        .rerank_precision
        .or_else(|| RerankPrecision::from_str(&view.namespace.defaults.rerank_precision))
        .unwrap_or(RerankPrecision::Int8);

    let base_target = if rerank_scale == 0 {
        opts.topk
    } else {
        opts.topk.saturating_mul(rerank_scale.max(1))
    };
    let per_part_limit = base_target.max(opts.topk);
    let global_limit = base_target
        .saturating_mul(4)
        .max(per_part_limit)
        .max(opts.topk);

    let mut candidates = Vec::new();

    for part in &view.parts {
        let plan = plan_for_part(
            &view.namespace,
            part.k_trained,
            part.small_part_fallback,
            probe_fraction,
        );
        let live =
            LiveSet::from_deletes(&view.delete_parts, part.doc_id_range).map_err(Error::Context)?;
        let mut part_candidates =
            gather_part_candidates(part, query, &plan, &live, per_part_limit)?;
        candidates.append(&mut part_candidates);
    }

    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    if candidates.len() > global_limit {
        candidates.truncate(global_limit);
    }

    match rerank_precision {
        RerankPrecision::None => {
            if candidates.len() > opts.topk {
                candidates.truncate(opts.topk);
            }
            Ok(candidates)
        }
        RerankPrecision::Int8 => rerank_int8(query, &candidates, &view.parts, opts.topk).await,
        RerankPrecision::Fp32 => {
            let cap = opts
                .fp32_rerank_cap
                .unwrap_or_else(|| opts.topk.saturating_mul(5))
                .max(opts.topk);
            let mid = rerank_int8(query, &candidates, &view.parts, cap).await?;
            rerank_fp32(query, &mid, &view.parts, opts.topk).await
        }
    }
}

fn gather_part_candidates(
    part: &PartMetadata,
    query: &[f32],
    plan: &PartSearchPlan,
    live: &LiveSet,
    limit: usize,
) -> Result<Vec<Candidate>> {
    let doc_count = usize::try_from(part.n).map_err(|_| {
        Error::Message(format!(
            "part {} reports too many rows to fit in memory",
            part.part_id.0
        ))
    })?;
    if doc_count == 0 {
        return Ok(Vec::new());
    }

    let (meta, codes) = load_rabitq(part)?;
    if meta.rows != doc_count {
        return Err(Error::Message(format!(
            "part {} RaBitQ rows {} did not match part count {}",
            part.part_id.0, meta.rows, doc_count
        )));
    }

    if meta.dim != query.len() {
        return Err(Error::Message(format!(
            "query dimension {} did not match RaBitQ dimension {}",
            query.len(),
            meta.dim
        )));
    }

    let scores = score_with_rabitq(&meta, query, &codes);
    if scores.len() != doc_count {
        return Err(Error::Message(format!(
            "RaBitQ scoring for part {} produced {} rows but expected {}",
            part.part_id.0,
            scores.len(),
            doc_count
        )));
    }

    let limit = limit.min(doc_count).max(1);

    if plan.fallback {
        return gather_fallback_candidates(part, live, &scores, limit);
    }

    gather_ivf_candidates(part, live, plan, query, &scores, limit)
}

fn gather_fallback_candidates(
    part: &PartMetadata,
    live: &LiveSet,
    scores: &[f32],
    limit: usize,
) -> Result<Vec<Candidate>> {
    let mut candidates = Vec::with_capacity(scores.len().min(limit));
    for (idx, &score) in scores.iter().enumerate() {
        if let Some(doc_id) = doc_id_from_index(part, idx) {
            if live.contains(doc_id) {
                candidates.push(Candidate {
                    part_id: part.part_id.clone(),
                    doc_id,
                    local_idx: idx,
                    score,
                });
            }
        }
    }

    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    if candidates.len() > limit {
        candidates.truncate(limit);
    }
    Ok(candidates)
}

fn gather_ivf_candidates(
    part: &PartMetadata,
    live: &LiveSet,
    plan: &PartSearchPlan,
    query: &[f32],
    scores: &[f32],
    limit: usize,
) -> Result<Vec<Candidate>> {
    let centroids = load_centroids(part)?;
    if centroids.is_empty() {
        return Err(Error::Message(format!(
            "part {} did not provide IVF centroids",
            part.part_id.0
        )));
    }

    let probe_lists = select_probe_lists(&centroids, query, plan.nprobe);
    if probe_lists.is_empty() {
        return Ok(Vec::new());
    }

    let mut doc_indices = HashSet::new();
    for list_id in probe_lists {
        let list_path = Path::new(&part.paths.ilist_dir).join(format!("{list_id:05}.ilist"));
        let docs = decode_ilist(&list_path, list_id, part.dim)?;
        for doc_idx in docs {
            doc_indices.insert(doc_idx);
        }
    }

    let mut candidates = Vec::with_capacity(doc_indices.len().min(limit));
    for doc_idx in doc_indices {
        if let Some(doc_id) = doc_id_from_index(part, doc_idx) {
            if doc_idx < scores.len() && live.contains(doc_id) {
                candidates.push(Candidate {
                    part_id: part.part_id.clone(),
                    doc_id,
                    local_idx: doc_idx,
                    score: scores[doc_idx],
                });
            }
        }
    }

    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    if candidates.len() > limit {
        candidates.truncate(limit);
    }
    Ok(candidates)
}

fn doc_id_from_index(part: &PartMetadata, idx: usize) -> Option<DocId> {
    let base = part.doc_id_range.0;
    let end = part.doc_id_range.1;
    if end < base {
        return None;
    }
    let doc_id = base.checked_add(idx as u64)?;
    if doc_id > end {
        None
    } else {
        Some(doc_id)
    }
}

fn load_rabitq(part: &PartMetadata) -> Result<(RaBitQMeta, Vec<u8>)> {
    let meta_path = Path::new(&part.paths.rabitq_meta);
    let codes_path = Path::new(&part.paths.rabitq_codes);
    let meta: RaBitQMeta = read_json(meta_path)?;
    let codes = read_binary(codes_path)?;
    Ok((meta, codes))
}

fn load_centroids(part: &PartMetadata) -> Result<Vec<Vec<f32>>> {
    let path = Path::new(&part.paths.centroids);
    let bytes = read_binary(path)?;
    if part.dim == 0 {
        return Err(Error::from("part dimensionality must be positive"));
    }
    let dim = part.dim;
    let stride = dim * 4;
    if stride == 0 || bytes.len() % stride != 0 {
        return Err(Error::Message(format!(
            "centroids at {} did not align with dimension {}",
            path.display(),
            dim
        )));
    }
    let k = bytes.len() / stride;
    let mut centroids = Vec::with_capacity(k);
    for chunk in bytes.chunks(stride) {
        let mut centroid = Vec::with_capacity(dim);
        for value in chunk.chunks(4) {
            let arr: [u8; 4] = value.try_into().unwrap();
            centroid.push(f32::from_le_bytes(arr));
        }
        centroids.push(centroid);
    }
    Ok(centroids)
}

fn select_probe_lists(centroids: &[Vec<f32>], query: &[f32], nprobe: usize) -> Vec<usize> {
    let mut pairs: Vec<(usize, f32)> = centroids
        .iter()
        .enumerate()
        .map(|(idx, centroid)| (idx, l2_distance(query, centroid)))
        .collect();
    pairs.sort_by(|a, b| a.1.total_cmp(&b.1));
    let take = nprobe.min(pairs.len());
    pairs.truncate(take);
    pairs.into_iter().map(|(idx, _)| idx).collect()
}

fn l2_distance(lhs: &[f32], rhs: &[f32]) -> f32 {
    lhs.iter()
        .zip(rhs.iter())
        .map(|(a, b)| {
            let diff = a - b;
            diff * diff
        })
        .sum()
}

fn decode_ilist(path: &Path, expected_list: usize, expected_dim: usize) -> Result<Vec<usize>> {
    let bytes = read_binary(path)?;
    if bytes.len() < 20 {
        return Err(Error::Message(format!(
            "ilist {} was too small to contain a header",
            path.display()
        )));
    }

    let mut offset = 0;
    if &bytes[offset..offset + 4] != b"ILST" {
        return Err(Error::Message(format!(
            "ilist {} missing magic header",
            path.display()
        )));
    }
    offset += 4;

    let version = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    offset += 4;
    if version != 1 {
        return Err(Error::Message(format!(
            "ilist {} version {} not supported",
            path.display(),
            version
        )));
    }

    let list_id = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
    offset += 4;
    if list_id != expected_list {
        return Err(Error::Message(format!(
            "ilist {} reported list id {} but expected {}",
            path.display(),
            list_id,
            expected_list
        )));
    }

    let count = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
    offset += 4;
    let dim = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
    offset += 4;
    if dim != expected_dim {
        return Err(Error::Message(format!(
            "ilist {} stored dim {} but expected {}",
            path.display(),
            dim,
            expected_dim
        )));
    }

    let mut docs = Vec::with_capacity(count);
    let mut prev = 0usize;
    for idx in 0..count {
        let delta = read_vbyte(&bytes, &mut offset)? as usize;
        let doc = if idx == 0 { delta } else { prev + delta };
        docs.push(doc);
        prev = doc;
    }

    let codes_bits = count
        .checked_mul(dim)
        .ok_or_else(|| Error::from("ilist codes overflow"))?;
    let codes_len = (codes_bits + 7) / 8;
    if offset + codes_len + 16 > bytes.len() {
        return Err(Error::Message(format!(
            "ilist {} truncated before codes/footer",
            path.display()
        )));
    }
    offset += codes_len;

    let first_doc = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
    offset += 8;
    let stored_count = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
    if stored_count != count as u64 {
        return Err(Error::Message(format!(
            "ilist {} stored count {} but header reported {}",
            path.display(),
            stored_count,
            count
        )));
    }
    if count > 0 && docs[0] as u64 != first_doc {
        return Err(Error::Message(format!(
            "ilist {} footer first doc {} did not match decoded {}",
            path.display(),
            first_doc,
            docs[0]
        )));
    }

    Ok(docs)
}

fn read_vbyte(bytes: &[u8], offset: &mut usize) -> Result<u64> {
    let mut shift = 0usize;
    let mut value = 0u64;
    loop {
        if *offset >= bytes.len() {
            return Err(Error::from("unterminated vbyte encoding"));
        }
        let byte = bytes[*offset];
        *offset += 1;
        value |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
        if shift >= 64 {
            return Err(Error::from("vbyte exceeds u64 range"));
        }
    }
}

fn read_json<T>(path: &Path) -> Result<T>
where
    T: DeserializeOwned,
{
    let data = fs::read(path)
        .with_context(|| format!("failed to read {}", path.display()))
        .map_err(Error::Context)?;
    serde_json::from_slice(&data)
        .with_context(|| format!("failed to parse {}", path.display()))
        .map_err(Error::Context)
}

fn read_binary(path: &Path) -> Result<Vec<u8>> {
    fs::read(path)
        .with_context(|| format!("failed to read {}", path.display()))
        .map_err(Error::Context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{DocId, NamespaceConfig, NamespaceDefaults, PartId, PartMetadata};
    use futures::executor::block_on;
    use part_builder::build_part;
    use quant::score_with_rabitq;
    use tempfile::tempdir;

    fn part_metadata_from_artifacts(
        cfg: &NamespaceConfig,
        part_id: &str,
        doc_id_base: DocId,
        artifacts: &part_builder::PartArtifacts,
    ) -> PartMetadata {
        let span = artifacts
            .doc_id_range
            .1
            .saturating_sub(artifacts.doc_id_range.0);
        let doc_end = doc_id_base.saturating_add(span);

        PartMetadata {
            part_id: PartId(part_id.to_string()),
            n: artifacts.rabitq_meta.rows as u64,
            dim: cfg.dim,
            k_trained: artifacts.k_trained,
            small_part_fallback: artifacts.small_part_fallback,
            doc_id_range: (doc_id_base, doc_end),
            paths: artifacts.paths.clone(),
            stats: artifacts.stats.clone(),
        }
    }

    fn manifest_from_parts(cfg: &NamespaceConfig, parts: Vec<PartMetadata>) -> ManifestView {
        ManifestView {
            namespace: cfg.clone(),
            parts,
            delete_parts: Vec::new(),
            epoch: 1,
        }
    }

    #[test]
    fn fallback_search_without_rerank() {
        let cfg = NamespaceConfig {
            dim: 3,
            cluster_factor: 1.0,
            k_min: 1,
            k_max: 8,
            nprobe_cap: 8,
            defaults: NamespaceDefaults::recommended(),
        };

        let vectors = vec![
            vec![1.0, 0.0, 0.5],
            vec![0.2, 0.1, 0.0],
            vec![0.9, 0.1, 0.6],
        ];
        let query = vec![1.0, 0.0, 0.4];
        let tempdir = tempdir().expect("tempdir");
        let artifacts =
            block_on(build_part(&cfg, vectors.clone(), tempdir.path())).expect("build part");
        let part = part_metadata_from_artifacts(&cfg, "p1", 0, &artifacts);
        let view = manifest_from_parts(&cfg, vec![part]);

        let scores = score_with_rabitq(&artifacts.rabitq_meta, &query, &artifacts.rabitq_codes);
        let mut expected: Vec<_> = scores
            .iter()
            .enumerate()
            .map(|(idx, &score)| (idx as u64, score))
            .collect();
        expected.sort_by(|a, b| b.1.total_cmp(&a.1));

        let opts = {
            let mut opts = SearchOptions::new(2);
            opts.rerank_scale = Some(0);
            opts.rerank_precision = Some(RerankPrecision::None);
            opts
        };

        let results = block_on(search_namespace(&view, &query, opts)).expect("search success");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].doc_id, expected[0].0);
        assert_eq!(results[1].doc_id, expected[1].0);
    }

    #[test]
    fn rerank_int8_respects_quantised_scores() {
        let mut cfg = NamespaceConfig::with_dim(4);
        cfg.defaults.rerank_precision = "int8".to_string();
        let vectors = vec![
            vec![0.1, 0.2, 0.3, 0.4],
            vec![0.9, 0.8, 0.7, 0.6],
            vec![0.5, 0.6, 0.7, 0.8],
        ];
        let query = vec![0.6, 0.5, 0.4, 0.3];
        let tempdir = tempdir().expect("tempdir");
        let artifacts =
            block_on(build_part(&cfg, vectors.clone(), tempdir.path())).expect("build part");
        let part = part_metadata_from_artifacts(&cfg, "p1", 0, &artifacts);
        let view = manifest_from_parts(&cfg, vec![part]);

        let opts = SearchOptions::new(3);
        let results = block_on(search_namespace(&view, &query, opts)).expect("search success");
        assert_eq!(results.len(), 3);

        let weights: Vec<f32> = artifacts
            .int8_meta
            .scales
            .iter()
            .zip(query.iter())
            .map(|(&scale, &q)| scale * q / 127.0)
            .collect();
        let mut expected: Vec<_> = (0..artifacts.int8_meta.rows)
            .map(|row| {
                let mut score = 0.0f32;
                for (idx, weight) in weights.iter().enumerate() {
                    let code = artifacts.int8_vectors[row * cfg.dim + idx] as f32;
                    score += code * weight;
                }
                (row as u64, score)
            })
            .collect();
        expected.sort_by(|a, b| b.1.total_cmp(&a.1));
        expected.truncate(results.len());

        for (candidate, (expected_id, expected_score)) in results.iter().zip(expected.iter()) {
            assert_eq!(candidate.doc_id, *expected_id);
            assert!((candidate.score - expected_score).abs() < 1e-3);
        }
    }

    #[test]
    fn fp32_rerank_matches_exact_dot_product() {
        let mut cfg = NamespaceConfig::with_dim(64);
        cfg.cluster_factor = 1.0;
        cfg.defaults.rerank_precision = "fp32".to_string();
        cfg.defaults.rerank_scale = 2;

        let rows = 4000;
        let mut vectors = Vec::with_capacity(rows);
        for row in 0..rows {
            let mut vec = Vec::with_capacity(cfg.dim);
            for col in 0..cfg.dim {
                vec.push((row as f32 + col as f32 * 0.01) / 10.0);
            }
            vectors.push(vec);
        }
        let query = vectors[rows - 1].clone();

        let tempdir = tempdir().expect("tempdir");
        let artifacts =
            block_on(build_part(&cfg, vectors.clone(), tempdir.path())).expect("build part");
        assert!(!artifacts.small_part_fallback, "expected IVF path");
        let part = part_metadata_from_artifacts(&cfg, "p1", 0, &artifacts);
        let view = manifest_from_parts(&cfg, vec![part]);

        let mut opts = SearchOptions::new(5);
        opts.probe_fraction = Some(1.0);
        opts.rerank_precision = Some(RerankPrecision::Fp32);
        opts.rerank_scale = Some(10);
        opts.fp32_rerank_cap = Some(200);
        let results = block_on(search_namespace(&view, &query, opts)).expect("search success");
        assert_eq!(results.len(), 5);

        let mut previous = f32::MAX;
        for candidate in &results {
            let actual: f32 = vectors[candidate.doc_id as usize]
                .iter()
                .zip(query.iter())
                .map(|(&a, &b)| a * b)
                .sum();
            assert!((candidate.score - actual).abs() < 1e-3);
            assert!(candidate.score <= previous + 1e-3);
            previous = candidate.score;
        }
    }

    #[test]
    fn search_merges_candidates_across_parts() {
        let cfg = NamespaceConfig::with_dim(2);
        let tempdir = tempdir().expect("tempdir");

        let part_a_dir = tempdir.path().join("part-a");
        let vectors_a = vec![vec![0.9, 0.1], vec![0.75, 0.2]];
        let artifacts_a =
            block_on(build_part(&cfg, vectors_a.clone(), &part_a_dir)).expect("build part a");

        let part_a = part_metadata_from_artifacts(&cfg, "p1", 0, &artifacts_a);
        let next_doc_id = part_a.doc_id_range.1.saturating_add(1);

        let part_b_dir = tempdir.path().join("part-b");
        let vectors_b = vec![vec![0.2, 0.8], vec![0.1, 0.9]];
        let artifacts_b =
            block_on(build_part(&cfg, vectors_b.clone(), &part_b_dir)).expect("build part b");
        let part_b = part_metadata_from_artifacts(&cfg, "p2", next_doc_id, &artifacts_b);

        let view = manifest_from_parts(&cfg, vec![part_a, part_b]);

        let query = vec![0.8, 0.2];
        let mut opts = SearchOptions::new(3);
        opts.rerank_scale = Some(0);
        opts.rerank_precision = Some(RerankPrecision::None);

        let results = block_on(search_namespace(&view, &query, opts)).expect("search success");
        assert_eq!(results.len(), 3);

        let mut expected = Vec::new();
        let scores_a =
            score_with_rabitq(&artifacts_a.rabitq_meta, &query, &artifacts_a.rabitq_codes);
        for (idx, &score) in scores_a.iter().enumerate() {
            expected.push((idx as DocId, score));
        }
        let scores_b =
            score_with_rabitq(&artifacts_b.rabitq_meta, &query, &artifacts_b.rabitq_codes);
        for (idx, &score) in scores_b.iter().enumerate() {
            expected.push((next_doc_id + idx as DocId, score));
        }
        expected.sort_by(|a, b| b.1.total_cmp(&a.1));
        expected.truncate(results.len());

        for (candidate, (expected_id, expected_score)) in results.iter().zip(expected.iter()) {
            assert_eq!(candidate.doc_id, *expected_id);
            assert!((candidate.score - expected_score).abs() < 1e-3);
        }
    }
}
