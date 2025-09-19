#![allow(dead_code)]

//! Common types and shared utilities for the elacsym workspace.
//!
//! The goal of this crate is to centralise the fundamental data models that
//! are shared across API boundaries (HTTP handlers, manifest/storage layers,
//! and background jobs).  Keeping the types here lightweight and serialisable
//! makes it easy for the higher-level crates to cooperate without depending
//! directly on one another's implementation details.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Logical identifier for a namespace.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NamespaceName(pub String);

/// Logical identifier for a persisted part.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PartId(pub String);

/// Logical identifier for a delete-part (tombstone batch).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeletePartId(pub String);

/// Monotonically increasing epoch value published with each manifest revision.
pub type Epoch = u64;

/// Numeric identifier assigned to a document within a namespace.
pub type DocId = u64;

/// Namespace-level default search knobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamespaceDefaults {
    pub probe_fraction: f32,
    pub rerank_scale: usize,
    pub rerank_precision: String,
}

impl NamespaceDefaults {
    /// Returns the canonical defaults described in the design document.
    pub fn recommended() -> Self {
        Self {
            probe_fraction: 0.10,
            rerank_scale: 5,
            rerank_precision: "int8".to_string(),
        }
    }
}

/// Namespace configuration persisted in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamespaceConfig {
    pub dim: usize,
    pub cluster_factor: f32,
    pub k_min: usize,
    pub k_max: usize,
    pub nprobe_cap: usize,
    pub defaults: NamespaceDefaults,
}

impl NamespaceConfig {
    /// Convenience helper for creating a configuration with the documented defaults.
    pub fn with_dim(dim: usize) -> Self {
        Self {
            dim,
            cluster_factor: 1.0,
            k_min: 1,
            k_max: 65_536,
            nprobe_cap: 8_192,
            defaults: NamespaceDefaults::recommended(),
        }
    }
}

/// Frequently accessed S3 object locations for a part.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartPaths {
    pub centroids: String,
    pub ilist_dir: String,
    pub rabitq_meta: String,
    pub rabitq_codes: String,
    pub vec_int8_dir: String,
    pub vec_fp32_dir: String,
}

/// Lightweight statistics emitted when a part is published.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartStatistics {
    pub created_at: String,
    pub mean_norm: f32,
}

/// Metadata for an immutable part that can serve search traffic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartMetadata {
    pub part_id: PartId,
    pub n: u64,
    pub dim: usize,
    pub k_trained: usize,
    pub small_part_fallback: bool,
    pub doc_id_range: (DocId, DocId),
    pub paths: PartPaths,
    pub stats: PartStatistics,
}

/// Metadata describing a delete batch (tombstone) that must be applied at query time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeletePartMetadata {
    pub del_part_id: DeletePartId,
    pub kind: DeletePartKind,
    pub created_at: String,
    pub paths: DeletePartPaths,
}

/// Enumeration of supported delete encodings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeletePartKind {
    Bitmap,
    IdList,
}

/// Object keys backing a tombstone representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeletePartPaths {
    pub bitmap: Option<String>,
    pub ids: Option<String>,
}

/// Candidate document produced during the search pipeline.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Identifier of the part that owns this document.
    pub part_id: PartId,
    /// Logical document identifier within the namespace.
    pub doc_id: DocId,
    /// Zero-based index of the document within the owning part.
    pub local_idx: usize,
    /// Similarity score carried between search stages.
    pub score: f32,
}

/// Snapshot of the full manifest view for a namespace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestView {
    pub namespace: NamespaceConfig,
    pub parts: Vec<PartMetadata>,
    pub delete_parts: Vec<DeletePartMetadata>,
    pub epoch: Epoch,
}

/// Canonical error type shared across crates.
#[derive(Debug, Error)]
pub enum Error {
    /// A human readable validation or domain error.
    #[error("{0}")]
    Message(String),
    /// Wrapper around context-rich anyhow errors.
    #[error(transparent)]
    Context(#[from] anyhow::Error),
}

/// Ergonomic result alias using the shared error type.
pub type Result<T> = std::result::Result<T, Error>;

impl From<&str> for Error {
    fn from(value: &str) -> Self {
        Self::Message(value.to_string())
    }
}
