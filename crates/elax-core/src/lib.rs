//! Core query planner, execution, and consistency logic for Phase 1.

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Instant,
};

use anyhow::{anyhow, Context, Result};
use elax_erq::{
    self as erq, DistanceMetric as ErqDistanceMetric, EncodedVector as ErqEncodedVector,
    Model as ErqModel, TrainConfig as ErqTrainConfig,
};
use elax_ivf::{self as ivf, IvfModel, TrainParams};
use elax_store::{Document, LocalStore, NamespaceStore, WalBatch, WalPointer, WriteOp};
use metrics::{counter, gauge, histogram};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

const IVF_MIN_TRAINING_POINTS: usize = 8;
const IVF_MAX_LISTS: usize = 256;
const IVF_TRAIN_SEED: u64 = 20_240_921;
const IVF_MAX_ITERATIONS: usize = 50;
const IVF_TOLERANCE: f32 = 1e-4;
const ERQ_DEFAULT_COARSE_BITS: u8 = 1;
const ERQ_DEFAULT_RERANK_BITS: u8 = 8;

/// Registry that coordinates namespace lifecycles backed by the storage layer.
#[derive(Clone)]
pub struct NamespaceRegistry {
    store: LocalStore,
    namespaces: Arc<RwLock<HashMap<String, Arc<NamespaceState>>>>,
}

impl NamespaceRegistry {
    pub fn new(store: LocalStore) -> Self {
        let _ = elax_metrics::init();
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
        let start = Instant::now();
        let pointer = ns.apply_write(&batch).await?;
        counter!(
            "elax_core_write_requests_total",
            1,
            "namespace" => namespace.clone()
        );
        histogram!(
            "elax_core_write_latency_seconds",
            start.elapsed().as_secs_f64(),
            "namespace" => namespace
        );
        Ok(pointer)
    }

    /// Execute a FP32 query with strong consistency semantics.
    pub async fn query(&self, request: QueryRequest) -> Result<QueryResponse> {
        let namespace = request.namespace.clone();
        let ns = self.namespace_state(&namespace).await?;
        let start = Instant::now();
        let response = ns.query(request).await?;
        counter!(
            "elax_core_query_requests_total",
            1,
            "namespace" => namespace.clone()
        );
        histogram!(
            "elax_core_query_latency_seconds",
            start.elapsed().as_secs_f64(),
            "namespace" => namespace
        );
        Ok(response)
    }

    /// Evaluate ANN recall against exhaustive search for the namespace.
    pub async fn debug_recall(&self, request: RecallRequest) -> Result<RecallResponse> {
        let ns = self.namespace_state(&request.namespace).await?;
        ns.debug_recall(request).await
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
        gauge!(
            "elax_core_rows_cached",
            guard.rows.len() as f64,
            "namespace" => batch.namespace.clone()
        );
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

    async fn debug_recall(&self, request: RecallRequest) -> Result<RecallResponse> {
        let mut guard = self.inner.write().await;
        guard.debug_recall(&request)
    }
}

struct NamespaceInner {
    config: NamespaceConfig,
    rows: HashMap<String, Row>,
    wal_highwater: u64,
    ivf: Option<IvfIndex>,
    ivf_dirty: bool,
}

impl NamespaceInner {
    fn new() -> Self {
        Self {
            config: NamespaceConfig::default(),
            rows: HashMap::new(),
            wal_highwater: 0,
            ivf: None,
            ivf_dirty: true,
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
        self.ivf_dirty = true;
        Ok(())
    }

    fn search(&mut self, request: &QueryRequest) -> Result<Vec<QueryHit>> {
        if request.top_k == 0 {
            return Ok(Vec::new());
        }
        let metric = request.metric.unwrap_or(self.config.distance_metric);
        let query_vec = &request.vector;
        if query_vec.is_empty() {
            return Err(anyhow!("query vector must not be empty"));
        }

        let mut ann_params = request.ann_params.clone();
        ann_params.fill_defaults();

        if ann_params.use_ivf {
            self.ensure_ivf(metric, &ann_params)?;
        }

        let mut results = self.ann_hits(query_vec, metric, request.top_k, &ann_params)?;
        if results.len() >= request.top_k {
            results.sort_by(compare_hits);
            results.truncate(request.top_k);
            return Ok(results);
        }

        let brute = self.brute_force_search(query_vec, metric, request.top_k)?;
        if results.is_empty() {
            return Ok(brute);
        }

        let mut seen: HashSet<String> = results.iter().map(|hit| hit.id.clone()).collect();
        for hit in brute {
            if seen.insert(hit.id.clone()) {
                results.push(hit);
            }
        }
        results.sort_by(compare_hits);
        results.truncate(request.top_k);
        Ok(results)
    }

    fn ensure_ivf(&mut self, metric: DistanceMetric, ann: &AnnParams) -> Result<()> {
        let coarse_bits = ann.coarse_bits.unwrap_or(ERQ_DEFAULT_COARSE_BITS);
        let rerank_bits = ann.rerank_bits.unwrap_or(ERQ_DEFAULT_RERANK_BITS);
        let needs_rebuild = self.ivf_dirty
            || self
                .ivf
                .as_ref()
                .map(|index| !index.matches(metric, coarse_bits, rerank_bits))
                .unwrap_or(true);
        if needs_rebuild {
            self.rebuild_ivf(metric, coarse_bits, rerank_bits)?;
        }
        Ok(())
    }

    fn ann_hits(
        &mut self,
        query_vec: &[f32],
        metric: DistanceMetric,
        top_k: usize,
        ann: &AnnParams,
    ) -> Result<Vec<QueryHit>> {
        if !ann.use_ivf || top_k == 0 {
            return Ok(Vec::new());
        }
        if let Some(index) = self.ivf.as_ref() {
            index.search(query_vec, metric, top_k, ann, &self.rows)
        } else {
            Ok(Vec::new())
        }
    }

    fn debug_recall(&mut self, request: &RecallRequest) -> Result<RecallResponse> {
        let metric = request.metric.unwrap_or(self.config.distance_metric);
        let mut ann_params = request.ann_params.clone();
        ann_params.fill_defaults();

        if ann_params.use_ivf {
            self.ensure_ivf(metric, &ann_params)?;
        }

        let target = request.num.max(1);
        let mut queries: Vec<Vec<f32>> = if let Some(explicit) = request.queries.clone() {
            explicit.into_iter().take(target).collect()
        } else {
            self.rows
                .values()
                .filter_map(|row| row.vector.clone())
                .take(target)
                .collect()
        };

        if queries.is_empty() {
            return Ok(RecallResponse::empty());
        }

        let top_k = request.top_k.max(1);
        let mut evaluated = 0usize;
        let mut total_recall = 0.0f32;
        let mut total_ann = 0usize;
        let mut total_brute = 0usize;

        for query in queries.drain(..) {
            if query.is_empty() {
                continue;
            }
            let ann_hits = match self.ann_hits(&query, metric, top_k, &ann_params) {
                Ok(hits) => hits,
                Err(_) => continue,
            };
            let brute_hits = match self.brute_force_search(&query, metric, top_k) {
                Ok(hits) => hits,
                Err(_) => continue,
            };
            if brute_hits.is_empty() {
                continue;
            }

            let brute_ids: HashSet<&str> = brute_hits.iter().map(|hit| hit.id.as_str()).collect();
            let matches = ann_hits
                .iter()
                .filter(|hit| brute_ids.contains(hit.id.as_str()))
                .count();
            let denom = brute_hits.len().min(top_k).max(1);
            total_recall += matches as f32 / denom as f32;
            total_ann += ann_hits.len();
            total_brute += brute_hits.len();
            evaluated += 1;
        }

        if evaluated == 0 {
            return Ok(RecallResponse::empty());
        }

        Ok(RecallResponse {
            avg_recall: total_recall / evaluated as f32,
            avg_ann_count: total_ann as f32 / evaluated as f32,
            avg_exhaustive_count: total_brute as f32 / evaluated as f32,
            evaluated,
        })
    }

    fn brute_force_search(
        &self,
        query_vec: &[f32],
        metric: DistanceMetric,
        top_k: usize,
    ) -> Result<Vec<QueryHit>> {
        let mut heap: Vec<(f32, &Row)> = Vec::new();
        for row in self.rows.values() {
            let Some(vector) = &row.vector else { continue };
            if vector.len() != query_vec.len() {
                continue;
            }
            let distance = metric.distance(query_vec, vector)?;
            heap.push((distance, row));
        }

        heap.sort_by(
            |a, b| match a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal) {
                Ordering::Equal => a.1.id.cmp(&b.1.id),
                other => other,
            },
        );
        heap.truncate(top_k);

        Ok(heap
            .into_iter()
            .map(|(score, row)| QueryHit {
                id: row.id.clone(),
                score,
                attributes: row.attributes.clone(),
            })
            .collect())
    }

    fn rebuild_ivf(
        &mut self,
        metric: DistanceMetric,
        coarse_bits: u8,
        rerank_bits: u8,
    ) -> Result<()> {
        self.ivf = self.build_ivf_index(metric, coarse_bits, rerank_bits)?;
        self.ivf_dirty = false;
        Ok(())
    }

    fn build_ivf_index(
        &self,
        metric: DistanceMetric,
        coarse_bits: u8,
        rerank_bits: u8,
    ) -> Result<Option<IvfIndex>> {
        let mut rows_with_vectors = Vec::new();
        for row in self.rows.values() {
            let Some(vector) = row.vector.as_ref() else {
                continue;
            };
            if vector.is_empty() {
                continue;
            }
            rows_with_vectors.push((row.id.clone(), vector.clone()));
        }

        if rows_with_vectors.len() < IVF_MIN_TRAINING_POINTS {
            return Ok(None);
        }

        let dim = rows_with_vectors[0].1.len();
        if dim == 0 {
            return Ok(None);
        }

        rows_with_vectors.retain(|(_, vector)| vector.len() == dim);
        if rows_with_vectors.len() < IVF_MIN_TRAINING_POINTS {
            return Ok(None);
        }

        let samples = rows_with_vectors
            .iter()
            .map(|(_, vector)| vector.clone())
            .collect::<Vec<Vec<f32>>>();

        let nlist = compute_nlist(samples.len());
        if nlist == 0 {
            return Ok(None);
        }

        if coarse_bits == 0 {
            return Err(anyhow!("coarse bits must be > 0"));
        }
        if rerank_bits == 0 {
            return Err(anyhow!("rerank bits must be > 0"));
        }
        if rerank_bits < coarse_bits {
            return Err(anyhow!(
                "rerank bits ({rerank_bits}) must be >= coarse bits ({coarse_bits})"
            ));
        }

        let params = TrainParams {
            nlist,
            max_iterations: IVF_MAX_ITERATIONS,
            tolerance: IVF_TOLERANCE,
            metric: to_ivf_metric(metric),
            seed: IVF_TRAIN_SEED,
        };

        let model = ivf::train(&samples, params)?;
        let mut postings = vec![Vec::new(); model.centroids().len()];
        for (id, vector) in rows_with_vectors.iter() {
            let assignment = model.assign(vector)?;
            if let Some(list) = postings.get_mut(assignment.list_id) {
                list.push(id.clone());
            }
        }

        let erq_model = erq::train(
            &samples,
            ErqTrainConfig {
                coarse_bits,
                fine_bits: rerank_bits,
            },
        )?;
        let mut encodings: HashMap<String, ErqEncodedVector> = HashMap::new();
        for (id, vector) in rows_with_vectors.iter() {
            let encoding = erq_model.encode(vector)?;
            encodings.insert(id.clone(), encoding);
        }

        Ok(Some(IvfIndex::new(
            model,
            postings,
            metric,
            dim,
            erq_model,
            encodings,
            coarse_bits,
            rerank_bits,
        )))
    }
}

fn compute_nlist(samples: usize) -> usize {
    if samples == 0 {
        return 0;
    }
    let approx = (samples as f32).sqrt().ceil() as usize;
    let capped = approx.max(1).min(samples).min(IVF_MAX_LISTS);
    capped.max(1)
}

struct IvfIndex {
    model: IvfModel,
    postings: Vec<Vec<String>>,
    metric: DistanceMetric,
    dimension: usize,
    erq: ErqModel,
    encodings: HashMap<String, ErqEncodedVector>,
    coarse_bits: u8,
    rerank_bits: u8,
}

impl IvfIndex {
    #[allow(clippy::too_many_arguments)]
    fn new(
        model: IvfModel,
        postings: Vec<Vec<String>>,
        metric: DistanceMetric,
        dimension: usize,
        erq: ErqModel,
        encodings: HashMap<String, ErqEncodedVector>,
        coarse_bits: u8,
        rerank_bits: u8,
    ) -> Self {
        Self {
            model,
            postings,
            metric,
            dimension,
            erq,
            encodings,
            coarse_bits,
            rerank_bits,
        }
    }

    fn matches(&self, metric: DistanceMetric, coarse_bits: u8, rerank_bits: u8) -> bool {
        self.metric == metric
            && self.dimension > 0
            && self.coarse_bits == coarse_bits
            && self.rerank_bits == rerank_bits
    }

    fn search(
        &self,
        query: &[f32],
        metric: DistanceMetric,
        top_k: usize,
        ann: &AnnParams,
        rows: &HashMap<String, Row>,
    ) -> Result<Vec<QueryHit>> {
        if top_k == 0 {
            return Ok(Vec::new());
        }
        if metric != self.metric {
            return Ok(Vec::new());
        }
        if query.len() != self.dimension {
            return Ok(Vec::new());
        }
        if self.postings.is_empty() {
            return Ok(Vec::new());
        }

        let nlist = self.postings.len();
        let nprobe = ann
            .nprobe
            .filter(|value| *value > 0)
            .map(|value| value.min(nlist))
            .unwrap_or_else(|| ivf::nprobe_for_recall(ann.target_recall, nlist));
        if nprobe == 0 {
            return Ok(Vec::new());
        }

        let probes = self.model.probe_order(query, nprobe)?;
        let mut coarse_candidates: Vec<(f32, &Row)> = Vec::new();
        for assignment in probes {
            if let Some(list) = self.postings.get(assignment.list_id) {
                for id in list {
                    let Some(row) = rows.get(id) else { continue };
                    let Some(encoding) = self.encodings.get(id) else {
                        continue;
                    };
                    let coarse_distance = self.erq.coarse_distance(
                        query,
                        encoding.coarse(),
                        to_erq_metric(metric),
                    )?;
                    coarse_candidates.push((coarse_distance, row));
                }
            }
        }

        if coarse_candidates.is_empty() {
            return Ok(Vec::new());
        }

        coarse_candidates.sort_by(|a, b| match a.0.partial_cmp(&b.0) {
            Some(Ordering::Equal) | None => a.1.id.cmp(&b.1.id),
            Some(ordering) => ordering,
        });

        let candidate_budget = ann.candidate_budget(top_k).max(top_k);
        coarse_candidates.truncate(candidate_budget);

        let mut hits = Vec::new();
        for (_, row) in coarse_candidates.into_iter() {
            let Some(encoding) = self.encodings.get(&row.id) else {
                continue;
            };
            let score = match ann.rerank_mode {
                RerankMode::Erq => {
                    self.erq
                        .fine_distance(query, encoding, to_erq_metric(metric))?
                }
                RerankMode::Fp32 => {
                    let Some(vector) = row.vector.as_ref() else {
                        continue;
                    };
                    if vector.len() != query.len() {
                        continue;
                    }
                    metric.distance(query, vector)?
                }
            };
            hits.push(QueryHit {
                id: row.id.clone(),
                score,
                attributes: row.attributes.clone(),
            });
        }

        hits.sort_by(compare_hits);
        hits.truncate(top_k);
        Ok(hits)
    }
}

fn to_ivf_metric(metric: DistanceMetric) -> ivf::DistanceMetric {
    match metric {
        DistanceMetric::Cosine => ivf::DistanceMetric::Cosine,
        DistanceMetric::EuclideanSquared => ivf::DistanceMetric::EuclideanSquared,
    }
}

fn to_erq_metric(metric: DistanceMetric) -> ErqDistanceMetric {
    match metric {
        DistanceMetric::Cosine => ErqDistanceMetric::Cosine,
        DistanceMetric::EuclideanSquared => ErqDistanceMetric::EuclideanSquared,
    }
}

fn compare_hits(a: &QueryHit, b: &QueryHit) -> Ordering {
    match a.score.partial_cmp(&b.score) {
        Some(Ordering::Equal) | None => a.id.cmp(&b.id),
        Some(ordering) => ordering,
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
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DistanceMetric {
    #[default]
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
#[serde(default)]
pub struct AnnParams {
    pub use_ivf: bool,
    pub target_recall: f32,
    pub nprobe: Option<usize>,
    pub rerank_scale: usize,
    pub rerank_mode: RerankMode,
    pub coarse_bits: Option<u8>,
    pub rerank_bits: Option<u8>,
}

impl Default for AnnParams {
    fn default() -> Self {
        Self {
            use_ivf: true,
            target_recall: 0.1,
            nprobe: None,
            rerank_scale: 5,
            rerank_mode: RerankMode::Erq,
            coarse_bits: None,
            rerank_bits: None,
        }
    }
}

impl AnnParams {
    fn fill_defaults(&mut self) {
        if self.coarse_bits.is_none() {
            self.coarse_bits = Some(ERQ_DEFAULT_COARSE_BITS);
        }
        if self.rerank_bits.is_none() {
            self.rerank_bits = Some(ERQ_DEFAULT_RERANK_BITS);
        }
    }

    fn candidate_budget(&self, top_k: usize) -> usize {
        if self.rerank_scale == 0 {
            top_k
        } else {
            top_k.saturating_mul(self.rerank_scale)
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RerankMode {
    #[default]
    Erq,
    Fp32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryRequest {
    pub namespace: String,
    pub vector: Vec<f32>,
    pub top_k: usize,
    #[serde(default)]
    pub metric: Option<DistanceMetric>,
    #[serde(default)]
    pub min_wal_sequence: Option<u64>,
    #[serde(default)]
    pub ann_params: AnnParams,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecallRequest {
    pub namespace: String,
    #[serde(default = "default_recall_num")]
    pub num: usize,
    #[serde(default = "default_recall_top_k")]
    pub top_k: usize,
    #[serde(default)]
    pub queries: Option<Vec<Vec<f32>>>,
    #[serde(default)]
    pub ann_params: AnnParams,
    #[serde(default)]
    pub metric: Option<DistanceMetric>,
}

fn default_recall_num() -> usize {
    10
}

fn default_recall_top_k() -> usize {
    10
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecallResponse {
    pub avg_recall: f32,
    pub avg_ann_count: f32,
    pub avg_exhaustive_count: f32,
    pub evaluated: usize,
}

impl RecallResponse {
    fn empty() -> Self {
        Self {
            avg_recall: 0.0,
            avg_ann_count: 0.0,
            avg_exhaustive_count: 0.0,
            evaluated: 0,
        }
    }
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

    #[test]
    fn ann_params_candidate_budget_scales_with_multiplier() {
        let params = AnnParams::default();
        assert_eq!(params.candidate_budget(4), 20);

        let params = AnnParams {
            rerank_scale: 0,
            ..AnnParams::default()
        };
        assert_eq!(params.candidate_budget(4), 4);
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
                ann_params: Default::default(),
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
                ann_params: Default::default(),
            })
            .await
            .expect("query");
        assert!(response.hits.is_empty());
    }

    #[tokio::test]
    async fn ivf_query_prefers_near_cluster() {
        let store = sample_store();
        let registry = NamespaceRegistry::new(store);
        let mut docs = Vec::new();
        for i in 0..16 {
            let position = -1.0 + i as f32 * 0.05;
            docs.push(doc(&format!("left-{i}"), &[position, 0.0]));
        }
        for i in 0..16 {
            let position = 10.0 + i as f32 * 0.05;
            docs.push(doc(&format!("right-{i}"), &[position, 0.0]));
        }

        registry
            .apply_write(WriteBatch {
                namespace: "ns".to_string(),
                upserts: docs,
                deletes: Vec::new(),
            })
            .await
            .expect("write");

        let response = registry
            .query(QueryRequest {
                namespace: "ns".to_string(),
                vector: vec![-0.5, 0.0],
                top_k: 3,
                metric: None,
                min_wal_sequence: None,
                ann_params: AnnParams {
                    use_ivf: true,
                    target_recall: 0.2,
                    nprobe: Some(1),
                    ..Default::default()
                },
            })
            .await
            .expect("query");

        assert!(!response.hits.is_empty(), "ivf should return candidates");
        assert!(
            response.hits[0].id.starts_with("left-"),
            "expected nearest cluster to win"
        );
    }

    #[tokio::test]
    async fn query_respects_min_wal_sequence() {
        let store = sample_store();
        let registry = NamespaceRegistry::new(store.clone());
        let pointer = registry
            .apply_write(WriteBatch {
                namespace: "ns".to_string(),
                upserts: vec![doc("a", &[1.0, 0.0])],
                deletes: Vec::new(),
            })
            .await
            .expect("initial write");

        let err = registry
            .query(QueryRequest {
                namespace: "ns".to_string(),
                vector: vec![1.0, 0.0],
                top_k: 1,
                metric: None,
                min_wal_sequence: Some(pointer.sequence + 1),
                ann_params: Default::default(),
            })
            .await
            .expect_err("consistency requirement should fail");
        assert!(err.to_string().contains("consistency level unmet"));
    }

    #[tokio::test]
    async fn query_catches_up_with_recent_wal() {
        let store = sample_store();
        let registry = NamespaceRegistry::new(store.clone());
        let first_pointer = registry
            .apply_write(WriteBatch {
                namespace: "ns".to_string(),
                upserts: vec![doc("a", &[1.0, 0.0])],
                deletes: Vec::new(),
            })
            .await
            .expect("initial write");

        let namespace_store = store.namespace("ns".to_string());
        let wal_batch = WalBatch {
            namespace: "ns".to_string(),
            operations: vec![WriteOp::Upsert {
                document: doc("b", &[0.0, 1.0]),
            }],
        };
        let second_pointer = namespace_store
            .append_batch(&wal_batch)
            .await
            .expect("external append");
        assert!(second_pointer.sequence > first_pointer.sequence);

        let response = registry
            .query(QueryRequest {
                namespace: "ns".to_string(),
                vector: vec![0.0, 1.0],
                top_k: 1,
                metric: None,
                min_wal_sequence: Some(second_pointer.sequence),
                ann_params: Default::default(),
            })
            .await
            .expect("query should reload WAL");

        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].id, "b");
    }

    #[tokio::test]
    async fn query_uses_brute_force_when_ivf_disabled() {
        let store = sample_store();
        let registry = NamespaceRegistry::new(store);
        let docs: Vec<Document> = (0..16)
            .map(|i| {
                let position = 1.0 + i as f32;
                doc(&format!("doc-{i}"), &[1.0, position])
            })
            .collect();
        registry
            .apply_write(WriteBatch {
                namespace: "ns".to_string(),
                upserts: docs,
                deletes: Vec::new(),
            })
            .await
            .expect("bulk write");

        let response = registry
            .query(QueryRequest {
                namespace: "ns".to_string(),
                vector: vec![1.0, 25.0],
                top_k: 1,
                metric: None,
                min_wal_sequence: None,
                ann_params: AnnParams {
                    use_ivf: false,
                    ..Default::default()
                },
            })
            .await
            .expect("brute-force query");

        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].id, "doc-15");
    }

    #[tokio::test]
    async fn debug_recall_reports_full_recall_with_fp32_rerank() {
        let store = sample_store();
        let registry = NamespaceRegistry::new(store);
        let docs: Vec<Document> = (0..12)
            .map(|i| {
                let x = i as f32;
                doc(&format!("doc-{i}"), &[x, 1.0 - x])
            })
            .collect();
        registry
            .apply_write(WriteBatch {
                namespace: "ns".to_string(),
                upserts: docs,
                deletes: Vec::new(),
            })
            .await
            .expect("seed data");

        let response = registry
            .debug_recall(RecallRequest {
                namespace: "ns".to_string(),
                num: 6,
                top_k: 3,
                queries: None,
                ann_params: AnnParams {
                    rerank_mode: RerankMode::Fp32,
                    target_recall: 1.0,
                    ..Default::default()
                },
                metric: None,
            })
            .await
            .expect("recall evaluation");

        assert_eq!(response.evaluated, 6);
        assert!((response.avg_recall - 1.0).abs() < 1e-6);
        assert!(response.avg_ann_count >= 3.0);
    }
}
