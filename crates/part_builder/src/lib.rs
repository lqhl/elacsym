#![allow(dead_code)]

//! Utilities for turning ingest batches into queryable parts.

use common::{Error, NamespaceConfig, Result};
use quant::RaBitQMeta;
use tracing::instrument;

/// Result of building a part prior to upload.
#[derive(Debug)]
pub struct PartArtifacts {
    pub rabitq_meta: RaBitQMeta,
}

/// Entry point for the ingest pipeline.
#[instrument]
pub async fn build_part(_cfg: &NamespaceConfig, _vectors: Vec<Vec<f32>>) -> Result<PartArtifacts> {
    Err(Error::Message(
        "part building is not yet implemented".to_string(),
    ))
}
