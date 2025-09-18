#![allow(dead_code)]

//! Stage-2 reranking over int8 or fp32 representations.

use common::{Error, Result};
use tracing::instrument;

/// Rerank candidates purely using int8 vectors.
#[instrument]
pub async fn rerank_int8() -> Result<()> {
    Err(Error::Message(
        "int8 reranking has not been implemented yet".to_string(),
    ))
}

/// Rerank candidates using fp32 vectors, optionally seeded by an int8 pass.
#[instrument]
pub async fn rerank_fp32() -> Result<()> {
    Err(Error::Message(
        "fp32 reranking has not been implemented yet".to_string(),
    ))
}
