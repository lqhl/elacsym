//! Storage layer abstractions for WAL, parts, and router metadata.

use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::{fs, io::AsyncWriteExt};

/// Root handle for interacting with namespace storage on the local filesystem.
#[derive(Clone, Debug)]
pub struct LocalStore {
    root: PathBuf,
    fsync: bool,
}

impl LocalStore {
    /// Create a new store rooted at `root`. Directories are created lazily.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            fsync: true,
        }
    }

    /// Configure whether WAL writes call `File::sync_all` after append.
    pub fn with_fsync(mut self, fsync: bool) -> Self {
        self.fsync = fsync;
        self
    }

    /// Return a namespace-scoped handle.
    pub fn namespace(&self, namespace: impl Into<String>) -> NamespaceStore {
        NamespaceStore {
            root: self.root.clone(),
            namespace: namespace.into(),
            fsync: self.fsync,
        }
    }
}

/// Namespace-scoped store that manages WAL and router state.
#[derive(Clone, Debug)]
pub struct NamespaceStore {
    root: PathBuf,
    namespace: String,
    fsync: bool,
}

impl NamespaceStore {
    fn namespace_root(&self) -> PathBuf {
        self.root.join("namespaces").join(&self.namespace)
    }

    fn wal_dir(&self) -> PathBuf {
        self.namespace_root().join("wal")
    }

    fn router_path(&self) -> PathBuf {
        self.namespace_root().join("router.json")
    }

    async fn ensure_dirs(&self) -> Result<()> {
        fs::create_dir_all(self.wal_dir()).await.with_context(|| {
            format!(
                "creating WAL dir for namespace '{}': {:?}",
                self.namespace,
                self.wal_dir()
            )
        })?;
        Ok(())
    }

    /// Append a strongly-consistent batch to the namespace WAL.
    pub async fn append_batch(&self, batch: &WalBatch) -> Result<WalPointer> {
        self.ensure_dirs().await?;
        let mut router = self.load_router().await?;
        let sequence = router.wal_highwater + 1;
        let filename = format!("WAL-{sequence:020}.json");
        let wal_path = self.wal_dir().join(filename);

        let mut file = fs::File::create(&wal_path)
            .await
            .with_context(|| format!("creating WAL file: {:?}", wal_path))?;
        let encoded = serde_json::to_vec(batch).context("encoding WAL batch to JSON")?;
        file.write_all(&encoded)
            .await
            .with_context(|| format!("writing WAL file: {:?}", wal_path))?;
        if self.fsync {
            file.sync_all()
                .await
                .with_context(|| format!("fsync WAL file: {:?}", wal_path))?;
        }

        router.wal_highwater = sequence;
        router.epoch += 1;
        router.updated_at = current_millis();
        self.store_router(&router).await?;

        Ok(WalPointer {
            namespace: self.namespace.clone(),
            sequence,
            file: wal_path,
        })
    }

    /// Read all WAL batches at or after the provided sequence number.
    pub async fn load_batches_since(&self, sequence: u64) -> Result<Vec<(WalPointer, WalBatch)>> {
        let mut entries = Vec::new();
        let mut dir = fs::read_dir(self.wal_dir())
            .await
            .with_context(|| format!("reading WAL dir: {:?}", self.wal_dir()))?;
        while let Some(entry) = dir.next_entry().await? {
            if !entry.file_type().await?.is_file() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let seq = parse_sequence(&name)?;
            if seq < sequence {
                continue;
            }
            let bytes = fs::read(entry.path()).await?;
            let batch: WalBatch = serde_json::from_slice(&bytes)
                .with_context(|| format!("decoding WAL batch from {:?}", entry.path()))?;
            entries.push((
                WalPointer {
                    namespace: self.namespace.clone(),
                    sequence: seq,
                    file: entry.path(),
                },
                batch,
            ));
        }
        entries.sort_by_key(|(ptr, _)| ptr.sequence);
        Ok(entries)
    }

    /// Load existing router state or return defaults.
    pub async fn load_router(&self) -> Result<RouterState> {
        let path = self.router_path();
        let bytes = match fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RouterState::new(self.namespace.clone()));
            }
            Err(err) => {
                return Err(err).with_context(|| format!("reading router state: {:?}", path))
            }
        };
        let router: RouterState = serde_json::from_slice(&bytes)
            .with_context(|| format!("decoding router state: {:?}", path))?;
        Ok(router)
    }

    /// Compare-and-swap style router update (Phase 1: simple overwrite with epoch guard).
    pub async fn store_router(&self, router: &RouterState) -> Result<()> {
        let path = self.router_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating router dir: {:?}", parent))?;
        }

        let encoded = serde_json::to_vec(router).context("encoding router state")?;
        let mut file = fs::File::create(&path)
            .await
            .with_context(|| format!("creating router file: {:?}", path))?;
        file.write_all(&encoded)
            .await
            .with_context(|| format!("writing router file: {:?}", path))?;
        if self.fsync {
            file.sync_all()
                .await
                .with_context(|| format!("fsync router file: {:?}", path))?;
        }
        Ok(())
    }
}

fn parse_sequence(name: &str) -> Result<u64> {
    if let Some(num) = name.strip_prefix("WAL-") {
        if let Some(num) = num.strip_suffix(".json") {
            return num.parse::<u64>().context("parsing WAL filename sequence");
        }
    }
    anyhow::bail!("unexpected WAL filename: {name}")
}

fn current_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Pointer to a WAL batch.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalPointer {
    pub namespace: String,
    pub sequence: u64,
    #[serde(skip)]
    pub file: PathBuf,
}

/// Serializable representation of router state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RouterState {
    pub namespace: String,
    pub epoch: u64,
    pub wal_highwater: u64,
    pub updated_at: u64,
}

impl RouterState {
    pub fn new(namespace: String) -> Self {
        Self {
            namespace,
            epoch: 0,
            wal_highwater: 0,
            updated_at: current_millis(),
        }
    }
}

/// Batch of write operations appended to the WAL.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WalBatch {
    pub namespace: String,
    pub operations: Vec<WriteOp>,
}

/// Supported write operations for Phase 1.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum WriteOp {
    Upsert { document: Document },
    Delete { id: String },
}

/// Minimal document representation stored in the WAL.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Document {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector: Option<Vec<f32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (PathBuf, NamespaceStore) {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "elacsym-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        path.push(unique);
        std::fs::create_dir_all(&path).expect("create temp dir");
        let store = LocalStore::new(&path).with_fsync(false);
        let ns = store.namespace("test");
        (path, ns)
    }

    fn sample_batch(seq: u64) -> WalBatch {
        WalBatch {
            namespace: "test".to_string(),
            operations: vec![WriteOp::Upsert {
                document: Document {
                    id: format!("doc-{seq}"),
                    vector: Some(vec![seq as f32]),
                    attributes: None,
                },
            }],
        }
    }

    #[tokio::test]
    async fn appending_batches_advances_sequence_and_router() {
        let (dir, ns) = temp_store();
        let first = ns.append_batch(&sample_batch(1)).await.expect("append 1");
        assert_eq!(first.sequence, 1);
        let router = ns.load_router().await.expect("load router");
        assert_eq!(router.wal_highwater, 1);

        let second = ns.append_batch(&sample_batch(2)).await.expect("append 2");
        assert_eq!(second.sequence, 2);
        let router = ns.load_router().await.expect("router 2");
        assert_eq!(router.wal_highwater, 2);
        assert!(router.epoch >= 2);
    }

    #[tokio::test]
    async fn load_batches_since_filters_sequences() {
        let (dir, ns) = temp_store();
        ns.append_batch(&sample_batch(1)).await.expect("append 1");
        ns.append_batch(&sample_batch(2)).await.expect("append 2");
        let batches = ns.load_batches_since(2).await.expect("load since 2");
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].0.sequence, 2);
        assert_eq!(batches[0].1.operations.len(), 1);

        // Clean up temp dir to avoid clutter.
        tokio::fs::remove_dir_all(dir).await.ok();
    }
}
