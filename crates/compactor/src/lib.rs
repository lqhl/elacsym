#![allow(dead_code)]

//! Background compaction orchestration.

use common::{Error, Result};
use tracing::instrument;

/// Executes a single compaction cycle for the provided namespace.
#[instrument]
pub async fn compact_once(namespace: &str) -> Result<()> {
    let _ = namespace;
    Err(Error::Message(
        "compaction is not yet implemented".to_string(),
    ))
}
