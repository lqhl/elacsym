//! Command-line tooling for elacsym administration.

use std::collections::HashSet;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use elax_config::ServiceConfig;
use elax_indexer::{Indexer, IndexerConfig};
use elax_store::{LocalStore, WalBatch};
use serde_json::json;

#[derive(Parser, Debug)]
#[command(author, version, about = "Administrative tooling for elacsym clusters")]
struct Cli {
    /// Path to the workspace data root.
    #[arg(long, default_value = ".elacsym")]
    root: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Materialize pending WAL entries and run compaction if needed.
    Compact {
        /// Namespace to index.
        namespace: String,
    },
    /// Verify router state and part manifests for a namespace.
    Verify { namespace: String },
    /// Export WAL batches since an optional sequence number to stdout as JSON.
    ExportWal {
        namespace: String,
        #[arg(long, default_value_t = 0)]
        since: u64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path =
        std::env::var("ELAX_CONFIG").unwrap_or_else(|_| "configs/query-node.toml".to_string());
    let mut config = ServiceConfig::load(&config_path)
        .with_context(|| format!("loading config from {config_path}"))?;
    config.data_root = cli.root.into();
    config
        .object_store
        .resolve_filesystem_root(&config.data_root);

    let mut store = LocalStore::new(&config.data_root);
    let object_store = config
        .object_store
        .build()
        .context("initializing object-store backend")?;
    store = store.with_object_store(object_store.store, object_store.prefix);

    match cli.command {
        Command::Compact { namespace } => {
            let indexer = Indexer::new(store.clone(), IndexerConfig::default());
            let router = indexer.run_until_idle(&namespace).await?;
            println!(
                "compacted namespace '{}' to {} parts (indexed_wal={})",
                namespace,
                router.parts.len(),
                router.indexed_wal
            );
        }
        Command::Verify { namespace } => verify_namespace(&store, &namespace).await?,
        Command::ExportWal { namespace, since } => export_wal(&store, &namespace, since).await?,
    }

    Ok(())
}

async fn verify_namespace(store: &LocalStore, namespace: &str) -> Result<()> {
    let ns_store = store.namespace(namespace.to_string());
    let router = ns_store.load_router().await?;
    let manifests = ns_store.list_part_manifests().await?;

    let router_ids: HashSet<_> = router.parts.iter().map(|p| p.id.clone()).collect();
    let manifest_ids: HashSet<_> = manifests.iter().map(|p| p.id.clone()).collect();

    if !router_ids.is_subset(&manifest_ids) {
        return Err(anyhow!(
            "router references missing manifests: {:?}",
            router_ids.difference(&manifest_ids).collect::<Vec<_>>()
        ));
    }

    println!(
        "namespace '{}' verified: {} parts, wal_highwater={}, indexed_wal={}",
        namespace,
        router.parts.len(),
        router.wal_highwater,
        router.indexed_wal
    );
    Ok(())
}

async fn export_wal(store: &LocalStore, namespace: &str, since: u64) -> Result<()> {
    let ns_store = store.namespace(namespace.to_string());
    let batches = ns_store.load_batches_since(since).await?;
    for (pointer, batch) in batches {
        print_batch(&pointer.sequence, &batch)?;
    }
    Ok(())
}

fn print_batch(sequence: &u64, batch: &WalBatch) -> Result<()> {
    let payload = json!({
        "sequence": sequence,
        "operations": batch.operations,
    });
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}
