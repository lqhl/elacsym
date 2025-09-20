//! Background indexer and compaction workflows for materializing parts.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use elax_store::{LocalStore, PartManifest, RouterState};
use metrics::{counter, histogram};

static PART_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Configuration values controlling part sizing and retention.
#[derive(Debug, Clone)]
pub struct IndexerConfig {
    pub rows_per_part: usize,
    pub max_active_parts: usize,
}

impl Default for IndexerConfig {
    fn default() -> Self {
        Self {
            rows_per_part: 256,
            max_active_parts: 4,
        }
    }
}

/// Result of a single indexer run.
#[derive(Debug, Clone)]
pub struct RunResult {
    pub router: RouterState,
    pub processed_batches: usize,
    pub parts_created: usize,
    pub compactions: usize,
}

impl RunResult {
    pub fn did_work(&self) -> bool {
        self.processed_batches > 0 || self.parts_created > 0 || self.compactions > 0
    }
}

/// Indexer encapsulation that processes namespace WAL data into parts.
#[derive(Clone)]
pub struct Indexer {
    store: LocalStore,
    config: IndexerConfig,
}

impl Indexer {
    pub fn new(store: LocalStore, config: IndexerConfig) -> Self {
        Self { store, config }
    }

    /// Process new WAL batches and compact parts if necessary.
    pub async fn run_once(&self, namespace: &str) -> Result<RunResult> {
        let timer = Instant::now();
        let ns_store = self.store.namespace(namespace.to_string());
        let mut router = ns_store.load_router().await?;
        let mut batches = ns_store
            .load_batches_since(router.indexed_wal + 1)
            .await
            .with_context(|| format!("loading WAL for namespace {namespace}"))?;
        batches.retain(|(pointer, _)| pointer.sequence > router.indexed_wal);

        let mut processed_batches = 0usize;
        let mut parts_created = 0usize;
        let mut last_sequence = router.indexed_wal;
        let mut bucket_rows = 0usize;
        let mut bucket_start: Option<u64> = None;

        for (pointer, batch) in batches.into_iter() {
            processed_batches += 1;
            if batch.operations.is_empty() {
                continue;
            }
            last_sequence = pointer.sequence;
            bucket_rows += batch.operations.len();
            if bucket_start.is_none() {
                bucket_start = Some(pointer.sequence);
            }
            if bucket_rows >= self.config.rows_per_part {
                let manifest = self
                    .flush_part(
                        namespace,
                        &ns_store,
                        bucket_start.unwrap_or(pointer.sequence),
                        pointer.sequence,
                        bucket_rows,
                    )
                    .await?;
                parts_created += 1;
                router.parts.push(manifest);
                bucket_rows = 0;
                bucket_start = None;
            }
        }

        if bucket_rows > 0 {
            let start_seq = bucket_start.unwrap_or(last_sequence.max(router.indexed_wal));
            let manifest = self
                .flush_part(namespace, &ns_store, start_seq, last_sequence, bucket_rows)
                .await?;
            parts_created += 1;
            router.parts.push(manifest);
        }

        let mut router_dirty = false;
        if parts_created > 0 {
            router.parts.sort_by_key(|part| part.wal_start);
            router_dirty = true;
        }
        if processed_batches > 0 {
            router.indexed_wal = last_sequence.max(router.indexed_wal);
            router.epoch = router.epoch.saturating_add(1);
            router.updated_at = current_millis();
            router_dirty = true;
        }
        if router_dirty {
            ns_store.store_router(&router).await?;
        }

        let compactions = self
            .maybe_compact(namespace, &ns_store, &mut router)
            .await?;

        if compactions > 0 {
            ns_store.store_router(&router).await?;
            counter!(
                "elax_indexer_compactions_total",
                compactions as u64,
                "namespace" => namespace.to_string()
            );
        }

        histogram!(
            "elax_indexer_run_latency_seconds",
            timer.elapsed().as_secs_f64(),
            "namespace" => namespace.to_string()
        );

        Ok(RunResult {
            router,
            processed_batches,
            parts_created,
            compactions,
        })
    }

    /// Continue running the indexer until no additional work remains.
    pub async fn run_until_idle(&self, namespace: &str) -> Result<RouterState> {
        loop {
            let result = self.run_once(namespace).await?;
            if !result.did_work() {
                return Ok(result.router);
            }
        }
    }

    async fn flush_part(
        &self,
        namespace: &str,
        ns_store: &elax_store::NamespaceStore,
        wal_start: u64,
        wal_end: u64,
        rows: usize,
    ) -> Result<PartManifest> {
        let manifest = PartManifest::new(new_part_id(namespace), wal_start, wal_end, rows);
        ns_store.write_part_manifest(&manifest).await?;
        counter!(
            "elax_indexer_parts_total",
            1,
            "namespace" => namespace.to_string(),
            "stage" => "materialize"
        );
        Ok(manifest)
    }

    async fn maybe_compact(
        &self,
        namespace: &str,
        ns_store: &elax_store::NamespaceStore,
        router: &mut RouterState,
    ) -> Result<usize> {
        if router.parts.len() <= self.config.max_active_parts {
            return Ok(0);
        }
        let mut parts = router.parts.clone();
        parts.sort_by_key(|p| p.wal_start);
        let merged_rows: usize = parts.iter().map(|p| p.rows).sum();
        let merged_start = parts.first().map(|p| p.wal_start).unwrap_or(0);
        let merged_end = parts
            .last()
            .map(|p| p.wal_end)
            .unwrap_or(router.indexed_wal);
        let mut merged = PartManifest::new(
            new_part_id(namespace),
            merged_start,
            merged_end,
            merged_rows,
        );
        merged.compacted_from = parts.iter().map(|p| p.id.clone()).collect();
        ns_store.write_part_manifest(&merged).await?;
        for part in &parts {
            ns_store.remove_part_manifest(&part.id).await.ok();
        }
        router.parts = vec![merged];
        router.indexed_wal = merged_end;
        router.epoch = router.epoch.saturating_add(1);
        router.updated_at = current_millis();
        counter!(
            "elax_indexer_parts_total",
            1,
            "namespace" => namespace.to_string(),
            "stage" => "compact"
        );
        Ok(1)
    }
}

/// Convenience helper matching the previous Phase 2 API.
pub async fn run_indexer(store: &LocalStore, namespace: &str) -> Result<RouterState> {
    let indexer = Indexer::new(store.clone(), IndexerConfig::default());
    indexer.run_until_idle(namespace).await
}

fn new_part_id(namespace: &str) -> String {
    let ns = namespace.replace('/', "_");
    let counter = PART_COUNTER.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    format!("part-{ns}-{timestamp}-{counter:016}")
}

fn current_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use elax_store::{Document, LocalStore, WalBatch, WriteOp};

    fn temp_store() -> (tempfile::TempDir, LocalStore) {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let store = LocalStore::new(dir.path()).with_fsync(false);
        (dir, store)
    }

    fn sample_batch(namespace: &str, seq: u64, rows: usize) -> WalBatch {
        WalBatch {
            namespace: namespace.to_string(),
            operations: (0..rows)
                .map(|idx| WriteOp::Upsert {
                    document: Document {
                        id: format!("doc-{seq}-{idx}"),
                        vector: Some(vec![idx as f32]),
                        attributes: None,
                    },
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn run_once_materializes_parts_and_advances_router() {
        let (dir, store) = temp_store();
        let ns = store.namespace("ns");
        ns.append_batch(&sample_batch("ns", 1, 10))
            .await
            .expect("append batch");
        ns.append_batch(&sample_batch("ns", 2, 10))
            .await
            .expect("append batch");

        let indexer = Indexer::new(
            store.clone(),
            IndexerConfig {
                rows_per_part: 8,
                max_active_parts: 4,
            },
        );
        let result = indexer.run_once("ns").await.expect("run indexer");
        assert!(result.parts_created >= 1);
        assert!(result.router.indexed_wal >= 2);
        let manifests = ns.list_part_manifests().await.expect("list parts");
        assert!(!manifests.is_empty());
        drop(dir);
    }

    #[tokio::test]
    async fn compaction_merges_parts_when_limit_exceeded() {
        let (_dir, store) = temp_store();
        let ns = store.namespace("ns");
        for seq in 1..=6 {
            ns.append_batch(&sample_batch("ns", seq, 4))
                .await
                .expect("append");
        }
        let indexer = Indexer::new(
            store.clone(),
            IndexerConfig {
                rows_per_part: 4,
                max_active_parts: 2,
            },
        );
        let result = indexer.run_until_idle("ns").await.expect("run indexer");
        assert_eq!(result.parts.len(), 1);
        assert!(result.parts[0].compacted_from.len() >= 2);
    }
}
