//! Background indexer and compaction workflows.

use anyhow::Result;
use elax_store::ObjectStore;

/// Placeholder indexer harness that currently does nothing.
pub async fn run_indexer<S: ObjectStore + Sync>(store: &S) -> Result<()> {
    store.put("/noop", &[]).await?;
    Ok(())
}
