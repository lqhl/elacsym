//! Background indexer and compaction workflows.

use anyhow::Result;
use elax_store::{LocalStore, RouterState};

/// Placeholder indexer harness that currently touches router state to prove wiring.
pub async fn run_indexer(store: &LocalStore, namespace: &str) -> Result<RouterState> {
    let ns_store = store.namespace(namespace.to_string());
    let router = ns_store.load_router().await?;
    Ok(router)
}
