//! Core query planner, execution, and consistency logic for Phase 1.

use std::{collections::HashMap, sync::Arc};

use anyhow::{anyhow, Context, Result};
use elax_store::{Document, LocalStore, NamespaceStore, WalBatch, WalPointer, WriteOp};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Registry that coordinates namespace lifecycles backed by the storage layer.
#[derive(Clone)]
pub struct NamespaceRegistry {
    store: LocalStore,
    namespaces: Arc<RwLock<HashMap<String, Arc<NamespaceState>>>>,
}

impl NamespaceRegistry {
    pub fn new(store: LocalStore) -> Self {
        Self {
            store,
            namespaces: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn namespace_state(&self, namespace: &str) -> Result<Arc<NamespaceState>> {
        if let Some(existing) = self.namespaces.read().await.get(namespace).cloned() {
            return Ok(existing);
        }

        let mut guard = self.namespaces.write().await;
        if let Some(existing) = guard.get(namespace).cloned() {
            return Ok(existing);
        }

        let ns_store = self.store.namespace(namespace.to_string());
        let state = NamespaceState::load(ns_store).await?;
        let state = Arc::new(state);
        guard.insert(namespace.to_string(), state.clone());
        Ok(state)
    }

    /// Apply a write batch to the namespace, returning the WAL pointer for strong reads.
    pub async fn apply_write(&self, batch: WriteBatch) -> Result<WalPointer> {
        let namespace = batch.namespace.clone();
        let ns = self.namespace_state(&namespace).await?;
        ns.apply_write(&batch).await
    }

    /// Execute a FP32 query with strong consistency semantics.
    pub async fn query(&self, request: QueryRequest) -> Result<QueryResponse> {
        let ns = self.namespace_state(&request.namespace).await?;
        ns.query(request).await
    }
}

struct NamespaceState {
    store: NamespaceStore,
    inner: RwLock<NamespaceInner>,
}

impl NamespaceState {
    async fn load(store: NamespaceStore) -> Result<Self> {
        let router = store.load_router().await?;
        let mut inner = NamespaceInner::new();
        let batches = store.load_batches_since(0).await?;
        for (pointer, batch) in batches {
            inner.apply_batch(&batch)?;
            inner.wal_highwater = pointer.sequence;
        }
        if inner.wal_highwater < router.wal_highwater {
            inner.wal_highwater = router.wal_highwater;
        }
        Ok(Self {
            store,
            inner: RwLock::new(inner),
        })
    }

    async fn apply_write(&self, batch: &WriteBatch) -> Result<WalPointer> {
        if batch.namespace.is_empty() {
            return Err(anyhow!("namespace is required"));
        }
        let wal_batch = batch.to_wal_batch();
        let pointer = self.store.append_batch(&wal_batch).await?;

        let mut guard = self.inner.write().await;
        guard.apply_batch(&wal_batch)?;
        guard.wal_highwater = pointer.sequence;
        Ok(pointer)
    }

    async fn query(&self, request: QueryRequest) -> Result<QueryResponse> {
        let mut guard = self.inner.write().await;
        if let Some(min_seq) = request.min_wal_sequence {
            if guard.wal_highwater < min_seq {
                // Reload from storage to catch up.
                let batches = self
                    .store
                    .load_batches_since(guard.wal_highwater + 1)
                    .await
                    .context("refreshing namespace state")?;
                for (pointer, batch) in batches {
                    guard.apply_batch(&batch)?;
                    guard.wal_highwater = pointer.sequence;
                }
            }

            if guard.wal_highwater < min_seq {
                return Err(anyhow!(
                    "consistency level unmet: wal={}, required={}",
                    guard.wal_highwater,
                    min_seq
                ));
            }
        }

        let results = guard.search(&request)?;
        Ok(QueryResponse { hits: results })
    }
}

struct NamespaceInner {
    config: NamespaceConfig,
    rows: HashMap<String, Row>,
    wal_highwater: u64,
}

impl NamespaceInner {
    fn new() -> Self {
        Self {
            config: NamespaceConfig::default(),
            rows: HashMap::new(),
            wal_highwater: 0,
        }
    }

    fn apply_batch(&mut self, batch: &WalBatch) -> Result<()> {
        for op in &batch.operations {
            match op {
                WriteOp::Upsert { document } => {
                    let row = Row::from(document.clone());
                    self.rows.insert(row.id.clone(), row);
                }
                WriteOp::Delete { id } => {
                    self.rows.remove(id);
                }
            }
        }
        Ok(())
    }

    fn search(&self, request: &QueryRequest) -> Result<Vec<QueryHit>> {
        if request.top_k == 0 {
            return Ok(Vec::new());
        }
        let metric = request.metric.unwrap_or(self.config.distance_metric);
        let query_vec = &request.vector;
        if query_vec.is_empty() {
            return Err(anyhow!("query vector must not be empty"));
        }

        let mut heap: Vec<(f32, &Row)> = Vec::new();
        for row in self.rows.values() {
            let Some(vector) = &row.vector else { continue };
            if vector.len() != query_vec.len() {
                continue;
            }
            let distance = metric.distance(query_vec, vector)?;
            heap.push((distance, row));
        }

        heap.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        heap.truncate(request.top_k);

        Ok(heap
            .into_iter()
            .map(|(score, row)| QueryHit {
                id: row.id.clone(),
                score,
                attributes: row.attributes.clone(),
            })
            .collect())
    }
}

/// Namespace configuration relevant for Phase 1 execution.
#[derive(Clone, Copy, Debug)]
struct NamespaceConfig {
    distance_metric: DistanceMetric,
}

impl Default for NamespaceConfig {
    fn default() -> Self {
        Self {
            distance_metric: DistanceMetric::Cosine,
        }
    }
}

/// Supported distance metrics.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistanceMetric {
    Cosine,
    EuclideanSquared,
}

impl DistanceMetric {
    fn distance(self, a: &[f32], b: &[f32]) -> Result<f32> {
        match self {
            DistanceMetric::Cosine => cosine_distance(a, b),
            DistanceMetric::EuclideanSquared => euclidean_squared(a, b),
        }
    }
}

impl Default for DistanceMetric {
    fn default() -> Self {
        DistanceMetric::Cosine
    }
}

fn cosine_distance(a: &[f32], b: &[f32]) -> Result<f32> {
    if a.len() != b.len() {
        return Err(anyhow!("dimension mismatch"));
    }
    let mut dot = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return Ok(1.0);
    }
    Ok(1.0 - dot / (norm_a.sqrt() * norm_b.sqrt()))
}

fn euclidean_squared(a: &[f32], b: &[f32]) -> Result<f32> {
    if a.len() != b.len() {
        return Err(anyhow!("dimension mismatch"));
    }
    let mut sum = 0.0;
    for i in 0..a.len() {
        let diff = a[i] - b[i];
        sum += diff * diff;
    }
    Ok(sum)
}

#[derive(Clone, Debug)]
struct Row {
    id: String,
    vector: Option<Vec<f32>>,
    attributes: Option<serde_json::Value>,
}

impl From<Document> for Row {
    fn from(doc: Document) -> Self {
        Self {
            id: doc.id,
            vector: doc.vector,
            attributes: doc.attributes,
        }
    }
}

/// Write batch accepted by the core layer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WriteBatch {
    pub namespace: String,
    #[serde(default)]
    pub upserts: Vec<Document>,
    #[serde(default)]
    pub deletes: Vec<String>,
}

impl WriteBatch {
    fn to_wal_batch(&self) -> WalBatch {
        let mut ops = Vec::new();
        for doc in &self.upserts {
            ops.push(WriteOp::Upsert {
                document: doc.clone(),
            });
        }
        for id in &self.deletes {
            ops.push(WriteOp::Delete { id: id.clone() });
        }
        WalBatch {
            namespace: self.namespace.clone(),
            operations: ops,
        }
    }
}

/// Query request accepted by the core layer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryRequest {
    pub namespace: String,
    pub vector: Vec<f32>,
    pub top_k: usize,
    #[serde(default)]
    pub metric: Option<DistanceMetric>,
    #[serde(default)]
    pub min_wal_sequence: Option<u64>,
}

/// Query response containing scored hits.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryResponse {
    pub hits: Vec<QueryHit>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryHit {
    pub id: String,
    pub score: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_store() -> LocalStore {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "elax-core-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        LocalStore::new(path).with_fsync(false)
    }

    fn doc(id: &str, vector: &[f32]) -> Document {
        Document {
            id: id.to_string(),
            vector: Some(vector.to_vec()),
            attributes: None,
        }
    }

    #[tokio::test]
    async fn write_then_query_returns_row() {
        let store = sample_store();
        let registry = NamespaceRegistry::new(store);
        let batch = WriteBatch {
            namespace: "ns".to_string(),
            upserts: vec![doc("a", &[1.0, 0.0])],
            deletes: vec![],
        };
        let pointer = registry.apply_write(batch).await.expect("write");
        let response = registry
            .query(QueryRequest {
                namespace: "ns".to_string(),
                vector: vec![1.0, 0.0],
                top_k: 1,
                metric: None,
                min_wal_sequence: Some(pointer.sequence),
            })
            .await
            .expect("query");
        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].id, "a");
        assert!(response.hits[0].score <= 1e-6);
    }

    #[tokio::test]
    async fn deletes_remove_rows() {
        let store = sample_store();
        let registry = NamespaceRegistry::new(store);
        registry
            .apply_write(WriteBatch {
                namespace: "ns".to_string(),
                upserts: vec![doc("a", &[0.0, 1.0])],
                deletes: vec![],
            })
            .await
            .expect("write 1");
        registry
            .apply_write(WriteBatch {
                namespace: "ns".to_string(),
                upserts: vec![],
                deletes: vec!["a".to_string() as String],
            })
            .await
            .expect("delete");
        let response = registry
            .query(QueryRequest {
                namespace: "ns".to_string(),
                vector: vec![0.0, 1.0],
                top_k: 1,
                metric: None,
                min_wal_sequence: None,
            })
            .await
            .expect("query");
        assert!(response.hits.is_empty());
    }
}
