#![allow(dead_code)]

//! Stage-2 reranking over int8 or fp32 representations.

use std::collections::HashMap;
use std::convert::TryFrom;
use std::fs;
use std::path::Path;

use anyhow::Context;
use common::{Candidate, Error, PartId, PartMetadata, Result};
use tracing::instrument;

/// Rerank candidates purely using int8 vectors.
#[instrument(skip(query, candidates, parts))]
pub async fn rerank_int8(
    query: &[f32],
    candidates: &[Candidate],
    parts: &[PartMetadata],
    limit: usize,
) -> Result<Vec<Candidate>> {
    if candidates.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let mut cache: HashMap<PartId, Int8State> = HashMap::new();
    let mut rescored = Vec::with_capacity(candidates.len());

    for candidate in candidates {
        let state = if let Some(state) = cache.get(&candidate.part_id) {
            state
        } else {
            let part = find_part(parts, &candidate.part_id)?;
            let state = Int8State::new(part, query)?;
            cache.insert(candidate.part_id.clone(), state);
            cache.get(&candidate.part_id).expect("inserted state")
        };

        let score = state.score(candidate.local_idx)?;
        let mut updated = candidate.clone();
        updated.score = score;
        rescored.push(updated);
    }

    rescored.sort_by(|a, b| b.score.total_cmp(&a.score));
    let take = limit.min(rescored.len());
    rescored.truncate(take);
    Ok(rescored)
}

/// Rerank candidates using fp32 vectors, optionally seeded by an int8 pass.
#[instrument(skip(query, candidates, parts))]
pub async fn rerank_fp32(
    query: &[f32],
    candidates: &[Candidate],
    parts: &[PartMetadata],
    limit: usize,
) -> Result<Vec<Candidate>> {
    if candidates.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let mut cache: HashMap<PartId, Fp32State> = HashMap::new();
    let mut rescored = Vec::with_capacity(candidates.len());

    for candidate in candidates {
        let state = if let Some(state) = cache.get(&candidate.part_id) {
            state
        } else {
            let part = find_part(parts, &candidate.part_id)?;
            let state = Fp32State::new(part)?;
            cache.insert(candidate.part_id.clone(), state);
            cache.get(&candidate.part_id).expect("inserted state")
        };

        let score = state.score(query, candidate.local_idx)?;
        let mut updated = candidate.clone();
        updated.score = score;
        rescored.push(updated);
    }

    rescored.sort_by(|a, b| b.score.total_cmp(&a.score));
    let take = limit.min(rescored.len());
    rescored.truncate(take);
    Ok(rescored)
}

fn find_part<'a>(parts: &'a [PartMetadata], part_id: &PartId) -> Result<&'a PartMetadata> {
    parts
        .iter()
        .find(|part| &part.part_id == part_id)
        .ok_or_else(|| Error::Message(format!("candidate referenced unknown part {}", part_id.0)))
}

struct Int8State {
    dim: usize,
    rows: usize,
    vectors: Vec<i8>,
    weights: Vec<f32>,
}

impl Int8State {
    fn new(part: &PartMetadata, query: &[f32]) -> Result<Self> {
        if part.dim == 0 {
            return Err(Error::from("part dimensionality must be positive"));
        }

        if query.len() != part.dim {
            return Err(Error::Message(format!(
                "query dimension {} did not match part dimension {}",
                query.len(),
                part.dim
            )));
        }

        let dir = Path::new(&part.paths.vec_int8_dir);
        let scales_path = dir.join("scales.bin");
        let vectors_path = dir.join("vecpage-00000.bin");

        let scale_bytes = fs::read(&scales_path)
            .with_context(|| format!("failed to read {}", scales_path.display()))
            .map_err(Error::Context)?;
        if scale_bytes.len() != part.dim * 4 {
            return Err(Error::Message(format!(
                "scales at {} length {} did not match expected {}",
                scales_path.display(),
                scale_bytes.len(),
                part.dim * 4
            )));
        }
        let mut scales = Vec::with_capacity(part.dim);
        for chunk in scale_bytes.chunks(4) {
            let arr: [u8; 4] = chunk.try_into().unwrap();
            scales.push(f32::from_le_bytes(arr));
        }

        let vector_bytes = fs::read(&vectors_path)
            .with_context(|| format!("failed to read {}", vectors_path.display()))
            .map_err(Error::Context)?;
        if vector_bytes.is_empty() {
            return Err(Error::from("int8 vector page was empty"));
        }
        if vector_bytes.len() % part.dim != 0 {
            return Err(Error::Message(format!(
                "int8 vectors at {} length {} not divisible by dim {}",
                vectors_path.display(),
                vector_bytes.len(),
                part.dim
            )));
        }
        let rows = vector_bytes.len() / part.dim;
        let expected_rows = usize::try_from(part.n).map_err(|_| {
            Error::Message(format!(
                "part {} reported too many rows for int8 rerank",
                part.part_id.0
            ))
        })?;
        if rows != expected_rows {
            return Err(Error::Message(format!(
                "int8 vectors for part {} contained {} rows but manifest reported {}",
                part.part_id.0, rows, expected_rows
            )));
        }
        let vectors: Vec<i8> = vector_bytes.iter().map(|byte| *byte as i8).collect();

        let mut weights = Vec::with_capacity(part.dim);
        for (scale, &q) in scales.iter().zip(query.iter()) {
            weights.push(scale * q / 127.0);
        }

        Ok(Self {
            dim: part.dim,
            rows,
            vectors,
            weights,
        })
    }

    fn score(&self, row: usize) -> Result<f32> {
        if row >= self.rows {
            return Err(Error::Message(format!(
                "candidate referenced row {} but part only has {}",
                row, self.rows
            )));
        }
        let start = row
            .checked_mul(self.dim)
            .ok_or_else(|| Error::from("row index overflow"))?;
        let end = start + self.dim;
        if end > self.vectors.len() {
            return Err(Error::from("int8 vectors truncated"));
        }

        let mut acc = 0.0f32;
        for (code, weight) in self.vectors[start..end].iter().zip(self.weights.iter()) {
            acc += *code as f32 * weight;
        }
        Ok(acc)
    }
}

struct Fp32State {
    dim: usize,
    rows: usize,
    vectors: Vec<f32>,
}

impl Fp32State {
    fn new(part: &PartMetadata) -> Result<Self> {
        if part.dim == 0 {
            return Err(Error::from("part dimensionality must be positive"));
        }

        let dir = Path::new(&part.paths.vec_fp32_dir);
        let path = dir.join("vecpage-00000.bin");
        let bytes = fs::read(&path)
            .with_context(|| format!("failed to read {}", path.display()))
            .map_err(Error::Context)?;
        if bytes.is_empty() {
            return Err(Error::from("fp32 vector page was empty"));
        }
        if bytes.len() % (part.dim * 4) != 0 {
            return Err(Error::Message(format!(
                "fp32 vectors at {} length {} not divisible by dim {}",
                path.display(),
                bytes.len(),
                part.dim
            )));
        }
        let rows = bytes.len() / (part.dim * 4);
        let expected_rows = usize::try_from(part.n).map_err(|_| {
            Error::Message(format!(
                "part {} reported too many rows for fp32 rerank",
                part.part_id.0
            ))
        })?;
        if rows != expected_rows {
            return Err(Error::Message(format!(
                "fp32 vectors for part {} contained {} rows but manifest reported {}",
                part.part_id.0, rows, expected_rows
            )));
        }
        let mut vectors = Vec::with_capacity(rows * part.dim);
        for chunk in bytes.chunks(4) {
            let arr: [u8; 4] = chunk.try_into().unwrap();
            vectors.push(f32::from_le_bytes(arr));
        }

        Ok(Self {
            dim: part.dim,
            rows,
            vectors,
        })
    }

    fn score(&self, query: &[f32], row: usize) -> Result<f32> {
        if query.len() != self.dim {
            return Err(Error::Message(format!(
                "query dimension {} did not match part dimension {}",
                query.len(),
                self.dim
            )));
        }
        if row >= self.rows {
            return Err(Error::Message(format!(
                "candidate referenced row {} but part only has {}",
                row, self.rows
            )));
        }
        let start = row
            .checked_mul(self.dim)
            .ok_or_else(|| Error::from("row index overflow"))?;
        let end = start + self.dim;
        if end > self.vectors.len() {
            return Err(Error::from("fp32 vectors truncated"));
        }

        let mut acc = 0.0f32;
        for (value, &q) in self.vectors[start..end].iter().zip(query.iter()) {
            acc += value * q;
        }
        Ok(acc)
    }
}
