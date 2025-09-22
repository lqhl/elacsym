//! Core query planner, execution, and consistency logic for Phase 1.

use std::{
    cmp::Ordering,
    collections::{BTreeSet, HashMap, HashSet},
    convert::TryInto,
    sync::Arc,
    time::Instant,
};

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose, Engine as _};
use elax_erq::{
    self as erq, DistanceMetric as ErqDistanceMetric, EncodedVector as ErqEncodedVector,
    Model as ErqModel, TrainConfig as ErqTrainConfig,
};
use elax_filter::{FilterBitmap, FilterExpr};
use elax_fts::{SchemaConfig, SearchHit as Bm25SearchHit, TantivyIndex, TextFieldConfig};
use elax_ivf::{self as ivf, IvfModel, TrainParams};
use elax_store::{Document, LocalStore, NamespaceStore, WalBatch, WalPointer, WriteOp};
use half::f16;
use metrics::{counter, gauge, histogram};
use serde::{de, ser::SerializeTuple, Deserialize, Serialize};
use tantivy::{schema::Field as TantivyField, Document as TantivyDocument, IndexReader};
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
        let groups = guard.group_hits(&results, request.group_by.as_ref());
        Ok(QueryResponse {
            hits: results,
            groups,
        })
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
    fts: Option<FtsIndex>,
    fts_dirty: bool,
}

struct FtsIndex {
    index: TantivyIndex,
    reader: IndexReader,
    field_map: HashMap<String, TantivyField>,
}

impl FtsIndex {
    fn search(&self, field: Option<&str>, query: &str, top_k: usize) -> Result<Vec<Bm25SearchHit>> {
        if let Some(name) = field {
            if let Some(handle) = self.field_map.get(name) {
                return self
                    .index
                    .search_with_fields(&self.reader, query, top_k, Some(&[*handle]));
            } else {
                return Ok(Vec::new());
            }
        }

        self.index.search(&self.reader, query, top_k)
    }
}

impl NamespaceInner {
    fn new() -> Self {
        Self {
            config: NamespaceConfig::default(),
            rows: HashMap::new(),
            wal_highwater: 0,
            ivf: None,
            ivf_dirty: true,
            fts: None,
            fts_dirty: true,
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
        self.fts_dirty = true;
        Ok(())
    }

    fn search(&mut self, request: &QueryRequest) -> Result<Vec<QueryHit>> {
        if request.top_k == 0 {
            return Ok(Vec::new());
        }

        let metric = request.metric.unwrap_or(self.config.distance_metric);
        let mut ann_params = request.ann_params.clone();
        ann_params.fill_defaults();

        let filter_bitmap = self.build_filter_bitmap(request)?;
        if let Some(bitmap) = filter_bitmap.as_ref() {
            if bitmap.is_empty() {
                return Ok(Vec::new());
            }
        }

        let mut vector_query: Option<(&[f32], usize)> = None;
        if let Some(RankBy::VectorAnn { vector, .. }) = request.rank_by.as_ref() {
            vector_query = Some((vector.as_slice(), request.top_k));
        }
        for clause in &request.queries {
            if let RankBy::VectorAnn { vector, .. } = &clause.rank_by {
                vector_query = Some((vector.as_slice(), clause.top_k.unwrap_or(request.top_k)));
                break;
            }
        }

        let mut bm25_clauses: Vec<(&str, &str, usize)> = Vec::new();
        if let Some(RankBy::Bm25 { field, query }) = request.rank_by.as_ref() {
            bm25_clauses.push((field.as_str(), query.as_str(), request.top_k));
        }
        for clause in &request.queries {
            if let RankBy::Bm25 { field, query } = &clause.rank_by {
                bm25_clauses.push((
                    field.as_str(),
                    query.as_str(),
                    clause.top_k.unwrap_or(request.top_k),
                ));
            }
        }

        if vector_query.is_none() && bm25_clauses.is_empty() {
            return Err(anyhow!(
                "rank_by or queries must include a supported search clause"
            ));
        }

        let filter_bitmap_ref = filter_bitmap.as_ref();

        let mut vector_hits: Vec<QueryHit> = Vec::new();
        let mut vector_query_data: Option<Vec<f32>> = None;
        if let Some((query_vec, clause_top_k)) = vector_query {
            if query_vec.is_empty() {
                return Err(anyhow!("query vector must not be empty"));
            }
            let candidate_top_k = clause_top_k.max(request.top_k);
            vector_hits = self.vector_search(
                query_vec,
                metric,
                candidate_top_k,
                &ann_params,
                filter_bitmap_ref,
            )?;
            vector_query_data = Some(query_vec.to_vec());
        }

        let mut bm25_scores: HashMap<String, f32> = HashMap::new();
        if !bm25_clauses.is_empty() {
            self.ensure_fts()?;
            if let Some(fts) = self.fts.as_ref() {
                for (field, query, clause_top_k) in bm25_clauses {
                    let hits = fts.search(Some(field), query, clause_top_k)?;
                    for hit in hits {
                        if let Some(bitmap) = filter_bitmap_ref {
                            if !bitmap.contains(&hit.doc_id) {
                                continue;
                            }
                        }
                        bm25_scores
                            .entry(hit.doc_id.clone())
                            .and_modify(|existing| {
                                if hit.score > *existing {
                                    *existing = hit.score;
                                }
                            })
                            .or_insert(hit.score);
                    }
                }
            }
        }

        if vector_hits.is_empty() {
            if bm25_scores.is_empty() {
                return Ok(Vec::new());
            }
            let mut hits: Vec<QueryHit> = bm25_scores
                .into_iter()
                .map(|(doc_id, score)| {
                    let attributes = self
                        .rows
                        .get(&doc_id)
                        .and_then(|row| row.attributes.clone());
                    QueryHit {
                        id: doc_id,
                        score: -score,
                        attributes,
                    }
                })
                .collect();
            hits.sort_by(compare_hits);
            hits.truncate(request.top_k);
            return Ok(hits);
        }

        let mut hits_by_id: HashMap<String, QueryHit> = vector_hits
            .into_iter()
            .map(|hit| (hit.id.clone(), hit))
            .collect();

        if !bm25_scores.is_empty() {
            let query_vec = vector_query_data.as_ref().expect("vector data available");
            for (doc_id, bm25_score) in bm25_scores {
                if let Some(existing) = hits_by_id.get_mut(&doc_id) {
                    existing.score = apply_bm25_boost(existing.score, bm25_score);
                } else {
                    let attributes = self
                        .rows
                        .get(&doc_id)
                        .and_then(|row| row.attributes.clone());
                    let mut score = None;
                    if let Some(distance) =
                        self.vector_distance_for(&doc_id, query_vec.as_slice(), metric)?
                    {
                        score = Some(distance);
                    }
                    let base = score.unwrap_or(1.0);
                    let blended = apply_bm25_boost(base, bm25_score);
                    hits_by_id.insert(
                        doc_id.clone(),
                        QueryHit {
                            id: doc_id,
                            score: blended,
                            attributes,
                        },
                    );
                }
            }
        }

        let mut hits: Vec<QueryHit> = hits_by_id.into_values().collect();
        hits.sort_by(compare_hits);
        hits.truncate(request.top_k);
        Ok(hits)
    }

    fn build_filter_bitmap(&self, request: &QueryRequest) -> Result<Option<FilterBitmap>> {
        let mut bitmap = request
            .filter_bitmap_ids
            .as_ref()
            .map(|ids| FilterBitmap::from_ids(ids.iter().cloned()));

        if let Some(expr) = request.filters.as_ref() {
            let expr_bitmap = self.evaluate_filter(expr)?;
            bitmap = match bitmap {
                Some(mut existing) => {
                    existing.intersect(&expr_bitmap);
                    Some(existing)
                }
                None => Some(expr_bitmap),
            };
        }

        Ok(bitmap)
    }

    fn vector_search(
        &mut self,
        query_vec: &[f32],
        metric: DistanceMetric,
        top_k: usize,
        ann: &AnnParams,
        filter: Option<&FilterBitmap>,
    ) -> Result<Vec<QueryHit>> {
        if top_k == 0 {
            return Ok(Vec::new());
        }

        let plan = if let Some(bitmap) = filter {
            select_query_plan(
                self.rows.len(),
                bitmap.len(),
                ann.candidate_budget(top_k),
                ann.use_ivf,
            )
        } else {
            QueryPlan::VectorFirst
        };

        if matches!(plan, QueryPlan::FilterFirst) {
            return self.brute_force_search(query_vec, metric, top_k, filter);
        }

        if ann.use_ivf {
            self.ensure_ivf(metric, ann)?;
        }

        let mut results = self.ann_hits(query_vec, metric, top_k, ann)?;
        if let Some(bitmap) = filter {
            results.retain(|hit| bitmap.contains(&hit.id));
        }
        if results.len() >= top_k {
            results.sort_by(compare_hits);
            results.truncate(top_k);
            return Ok(results);
        }

        let brute = self.brute_force_search(query_vec, metric, top_k, filter)?;
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
        results.truncate(top_k);
        Ok(results)
    }

    fn ensure_fts(&mut self) -> Result<()> {
        if !self.fts_dirty {
            return Ok(());
        }

        let mut fields: BTreeSet<String> = BTreeSet::new();
        for row in self.rows.values() {
            let Some(attrs) = row.attributes.as_ref().and_then(|value| value.as_object()) else {
                continue;
            };
            for (key, value) in attrs {
                if value.is_string()
                    || value
                        .as_array()
                        .map(|items| items.iter().any(|item| item.is_string()))
                        .unwrap_or(false)
                {
                    fields.insert(key.clone());
                }
            }
        }

        if fields.is_empty() {
            self.fts = None;
            self.fts_dirty = false;
            return Ok(());
        }

        let mut config = SchemaConfig::new("doc_id");
        for field in &fields {
            config = config.add_text_field(TextFieldConfig::new(field).stored());
        }

        let index = TantivyIndex::create_in_ram(config)?;
        let mut writer = index
            .index_writer(50_000_000)
            .context("creating Tantivy index writer")?;
        for row in self.rows.values() {
            let mut doc = TantivyDocument::new();
            doc.add_text(index.id_field(), &row.id);
            if let Some(attrs) = row.attributes.as_ref().and_then(|value| value.as_object()) {
                for (key, value) in attrs {
                    let Some(field) = index.field(key) else {
                        continue;
                    };
                    match value {
                        serde_json::Value::String(text) => {
                            doc.add_text(field, text);
                        }
                        serde_json::Value::Array(items) => {
                            for item in items {
                                if let serde_json::Value::String(text) = item {
                                    doc.add_text(field, text);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            writer
                .add_document(doc)
                .context("adding document to Tantivy index")?;
        }
        writer
            .commit()
            .context("committing Tantivy index rebuild")?;
        let reader = index.reader()?;

        let mut field_map = HashMap::new();
        for field_name in fields {
            if let Some(field) = index.field(&field_name) {
                field_map.insert(field_name, field);
            }
        }

        self.fts = Some(FtsIndex {
            index,
            reader,
            field_map,
        });
        self.fts_dirty = false;
        Ok(())
    }

    fn vector_distance_for(
        &self,
        doc_id: &str,
        query: &[f32],
        metric: DistanceMetric,
    ) -> Result<Option<f32>> {
        let Some(row) = self.rows.get(doc_id) else {
            return Ok(None);
        };
        let Some(vector) = row.vector.as_ref() else {
            return Ok(None);
        };
        if vector.len() != query.len() {
            return Ok(None);
        }
        Ok(Some(metric.distance(query, vector)?))
    }

    fn evaluate_filter(&self, filter: &FilterExpr) -> Result<FilterBitmap> {
        let mut matches = Vec::new();
        for row in self.rows.values() {
            if elax_filter::evaluate(filter, row.attributes.as_ref(), Some(row.id.as_str()))? {
                matches.push(row.id.clone());
            }
        }
        Ok(FilterBitmap::from_ids(matches))
    }

    fn group_hits(
        &self,
        hits: &[QueryHit],
        group_by: Option<&GroupBy>,
    ) -> Option<Vec<GroupAggregation>> {
        let config = group_by?;
        if hits.is_empty() || config.limit == 0 {
            return None;
        }

        let mut groups: HashMap<String, GroupAccumulator> = HashMap::new();
        for hit in hits {
            let Some(row) = self.rows.get(&hit.id) else {
                continue;
            };
            let Some(attrs) = row.attributes.as_ref() else {
                continue;
            };
            let Some(group_value) = extract_group_value(attrs, &config.field) else {
                continue;
            };
            let entry = groups
                .entry(group_value.clone())
                .or_insert_with(|| GroupAccumulator::new(group_value.clone()));
            entry.observe(hit.clone(), config.per_group_limit);
        }

        if groups.is_empty() {
            return None;
        }

        let mut aggregations: Vec<GroupAggregation> =
            groups.into_values().map(GroupAggregation::from).collect();
        aggregations.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
        aggregations.truncate(config.limit);
        Some(aggregations)
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
            let brute_hits = match self.brute_force_search(&query, metric, top_k, None) {
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
        filter: Option<&FilterBitmap>,
    ) -> Result<Vec<QueryHit>> {
        let mut heap: Vec<(f32, &Row)> = Vec::new();
        for row in self.rows.values() {
            if let Some(bitmap) = filter {
                if !bitmap.contains(&row.id) {
                    continue;
                }
            }
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

fn apply_bm25_boost(base: f32, bm25_score: f32) -> f32 {
    let adjusted = base - bm25_score * 0.01;
    if adjusted.is_finite() {
        adjusted
    } else {
        base
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
    pub top_k: usize,
    #[serde(default)]
    pub rank_by: Option<RankBy>,
    #[serde(default)]
    pub queries: Vec<QueryClause>,
    #[serde(default)]
    pub metric: Option<DistanceMetric>,
    #[serde(default)]
    pub min_wal_sequence: Option<u64>,
    #[serde(default)]
    pub ann_params: AnnParams,
    #[serde(default)]
    pub group_by: Option<GroupBy>,
    #[serde(default)]
    pub filters: Option<FilterExpr>,
    #[serde(default)]
    pub filter_bitmap_ids: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RankBy {
    VectorAnn { field: String, vector: Vec<f32> },
    Bm25 { field: String, query: String },
}

impl RankBy {
    pub fn vector(field: impl Into<String>, vector: Vec<f32>) -> Self {
        Self::VectorAnn {
            field: field.into(),
            vector,
        }
    }

    pub fn bm25(field: impl Into<String>, query: impl Into<String>) -> Self {
        Self::Bm25 {
            field: field.into(),
            query: query.into(),
        }
    }
}

impl Serialize for RankBy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut seq = serializer.serialize_tuple(3)?;
        match self {
            RankBy::VectorAnn { field, vector } => {
                seq.serialize_element(field)?;
                seq.serialize_element("ANN")?;
                seq.serialize_element(vector)?;
            }
            RankBy::Bm25 { field, query } => {
                seq.serialize_element(field)?;
                seq.serialize_element("BM25")?;
                seq.serialize_element(query)?;
            }
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for RankBy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let values: Vec<serde_json::Value> = Vec::deserialize(deserializer)?;
        if values.len() != 3 {
            return Err(de::Error::custom(
                "rank_by must contain exactly three elements",
            ));
        }
        let field = values[0]
            .as_str()
            .ok_or_else(|| de::Error::custom("rank_by[0] must be a string field name"))?
            .to_string();
        let mode = values[1]
            .as_str()
            .ok_or_else(|| de::Error::custom("rank_by[1] must be a string mode"))?;
        match mode {
            "ANN" => parse_ann_vector(&values[2])
                .map(|vector| RankBy::VectorAnn { field, vector })
                .map_err(de::Error::custom),
            "BM25" => {
                let query = values[2]
                    .as_str()
                    .ok_or_else(|| de::Error::custom("rank_by BM25 payload must be a string"))?
                    .to_string();
                Ok(RankBy::Bm25 { field, query })
            }
            other => Err(de::Error::custom(format!(
                "unsupported rank_by mode: {other}"
            ))),
        }
    }
}

fn parse_ann_vector(value: &serde_json::Value) -> Result<Vec<f32>, String> {
    match value {
        serde_json::Value::Array(array) => {
            let mut vector = Vec::with_capacity(array.len());
            for value in array {
                let Some(number) = value.as_f64() else {
                    return Err("rank_by ANN payload must contain numeric values".to_string());
                };
                vector.push(number as f32);
            }
            Ok(vector)
        }
        serde_json::Value::String(text) => decode_base64_vector(text)
            .map_err(|err| format!("failed to decode rank_by ANN base64 payload: {err}")),
        other => Err(format!("unsupported rank_by ANN payload: {}", other)),
    }
}

#[derive(Clone, Copy)]
enum VectorPrecision {
    F16,
    F32,
}

fn decode_base64_vector(input: &str) -> Result<Vec<f32>, String> {
    let payload = input.strip_prefix("base64:").unwrap_or(input);
    let (precision, data) = if let Some(rest) = payload.strip_prefix("f16:") {
        (Some(VectorPrecision::F16), rest)
    } else if let Some(rest) = payload.strip_prefix("f32:") {
        (Some(VectorPrecision::F32), rest)
    } else {
        (None, payload)
    };

    let bytes = general_purpose::STANDARD
        .decode(data)
        .map_err(|err| format!("invalid base64 payload: {err}"))?;

    match precision {
        Some(VectorPrecision::F16) => decode_f16_bytes(&bytes),
        Some(VectorPrecision::F32) => decode_f32_bytes(&bytes),
        None => {
            if bytes.is_empty() {
                Ok(Vec::new())
            } else if bytes.len() % 4 == 0 {
                decode_f32_bytes(&bytes)
            } else if bytes.len() % 2 == 0 {
                decode_f16_bytes(&bytes)
            } else {
                Err(format!(
                    "base64 vector byte length {} is not compatible with f16 or f32",
                    bytes.len()
                ))
            }
        }
    }
}

fn decode_f32_bytes(bytes: &[u8]) -> Result<Vec<f32>, String> {
    if bytes.len() % 4 != 0 {
        return Err(format!(
            "expected byte length divisible by 4 for f32 payload, got {}",
            bytes.len()
        ));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| {
            let array: [u8; 4] = chunk
                .try_into()
                .expect("chunks_exact ensures chunk length is four bytes");
            f32::from_le_bytes(array)
        })
        .collect())
}

fn decode_f16_bytes(bytes: &[u8]) -> Result<Vec<f32>, String> {
    if bytes.len() % 2 != 0 {
        return Err(format!(
            "expected byte length divisible by 2 for f16 payload, got {}",
            bytes.len()
        ));
    }
    Ok(bytes
        .chunks_exact(2)
        .map(|chunk| {
            let array: [u8; 2] = chunk
                .try_into()
                .expect("chunks_exact ensures chunk length is two bytes");
            f16::from_bits(u16::from_le_bytes(array)).to_f32()
        })
        .collect())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryClause {
    pub rank_by: RankBy,
    #[serde(default)]
    pub top_k: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueryPlan {
    VectorFirst,
    FilterFirst,
}

fn select_query_plan(
    total_rows: usize,
    filtered_rows: usize,
    candidate_budget: usize,
    ann_enabled: bool,
) -> QueryPlan {
    if !ann_enabled {
        return QueryPlan::FilterFirst;
    }
    if filtered_rows == 0 || total_rows == 0 {
        return QueryPlan::FilterFirst;
    }
    if filtered_rows <= candidate_budget.max(1) {
        return QueryPlan::FilterFirst;
    }
    let selectivity = filtered_rows as f32 / total_rows as f32;
    if selectivity <= 0.2 {
        QueryPlan::FilterFirst
    } else {
        QueryPlan::VectorFirst
    }
}

/// Query response containing scored hits.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryResponse {
    pub hits: Vec<QueryHit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groups: Option<Vec<GroupAggregation>>,
}

/// Configuration describing how to group query hits.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupBy {
    /// Dot-delimited path to the attribute used for grouping.
    pub field: String,
    /// Maximum number of groups to return. Defaults to 10.
    #[serde(default = "GroupBy::default_group_limit")]
    pub limit: usize,
    /// Maximum number of representative hits to retain per group. Defaults to 1.
    #[serde(default = "GroupBy::default_per_group_limit")]
    pub per_group_limit: usize,
}

impl GroupBy {
    fn default_group_limit() -> usize {
        10
    }

    fn default_per_group_limit() -> usize {
        1
    }
}

/// Aggregated view of hits that share the same grouping value.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupAggregation {
    /// Group value extracted from the hit attributes.
    pub value: String,
    /// Total number of hits observed for this group.
    pub count: usize,
    /// Representative hits (ordered) retained for the group.
    pub hits: Vec<QueryHit>,
}

#[derive(Clone, Debug)]
struct GroupAccumulator {
    value: String,
    count: usize,
    hits: Vec<QueryHit>,
}

impl GroupAccumulator {
    fn new(value: String) -> Self {
        Self {
            value,
            count: 0,
            hits: Vec::new(),
        }
    }

    fn observe(&mut self, hit: QueryHit, per_group_limit: usize) {
        self.count += 1;
        if per_group_limit == 0 {
            return;
        }
        if self.hits.len() < per_group_limit {
            self.hits.push(hit);
        }
    }
}

impl From<GroupAccumulator> for GroupAggregation {
    fn from(accumulator: GroupAccumulator) -> Self {
        Self {
            value: accumulator.value,
            count: accumulator.count,
            hits: accumulator.hits,
        }
    }
}

fn extract_group_value(value: &serde_json::Value, field: &str) -> Option<String> {
    if field.is_empty() {
        return None;
    }
    let mut current = value;
    for segment in field.split('.') {
        if segment.is_empty() {
            return None;
        }
        match current {
            serde_json::Value::Object(map) => {
                current = map.get(segment)?;
            }
            serde_json::Value::Array(items) => {
                let index: usize = segment.parse().ok()?;
                current = items.get(index)?;
            }
            _ => return None,
        }
    }

    match current {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(num) => Some(num.to_string()),
        serde_json::Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
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
    use half::f16;
    use serde_json::json;

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

    fn vector_query(namespace: &str, vector: Vec<f32>, top_k: usize) -> QueryRequest {
        QueryRequest {
            namespace: namespace.to_string(),
            top_k,
            rank_by: Some(RankBy::vector("vector", vector)),
            queries: Vec::new(),
            metric: None,
            min_wal_sequence: None,
            ann_params: Default::default(),
            group_by: None,
            filters: None,
            filter_bitmap_ids: None,
        }
    }

    #[test]
    fn rank_by_vector_accepts_base64_f32_payload() {
        let values = [1.0f32, -2.5f32];
        let mut bytes = Vec::new();
        for value in values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        let payload = json!(["vector", "ANN", format!("base64:{encoded}")]);
        let parsed: RankBy = serde_json::from_value(payload).expect("parse rank_by");
        assert_eq!(parsed, RankBy::vector("vector", vec![1.0, -2.5]));
    }

    #[test]
    fn rank_by_vector_accepts_base64_f16_payload() {
        let values = [f16::from_f32(0.5), f16::from_f32(-1.0)];
        let mut bytes = Vec::new();
        for value in values {
            bytes.extend_from_slice(&value.to_bits().to_le_bytes());
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        let payload = json!(["vector", "ANN", format!("base64:f16:{encoded}")]);
        let parsed: RankBy = serde_json::from_value(payload).expect("parse rank_by");
        assert_eq!(parsed, RankBy::vector("vector", vec![0.5, -1.0]));
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

    #[test]
    fn planner_prefers_filter_first_for_selective_predicates() {
        assert_eq!(
            select_query_plan(1_000, 10, 50, true),
            QueryPlan::FilterFirst
        );
        assert_eq!(
            select_query_plan(1_000, 400, 50, true),
            QueryPlan::VectorFirst
        );
        assert_eq!(
            select_query_plan(1_000, 200, 220, true),
            QueryPlan::FilterFirst
        );
        assert_eq!(
            select_query_plan(1_000, 200, 50, false),
            QueryPlan::FilterFirst
        );
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
        let mut request = vector_query("ns", vec![1.0, 0.0], 1);
        request.min_wal_sequence = Some(pointer.sequence);
        let response = registry.query(request).await.expect("query");
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
            .query(vector_query("ns", vec![0.0, 1.0], 1))
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

        let mut request = vector_query("ns", vec![-0.5, 0.0], 3);
        request.ann_params = AnnParams {
            use_ivf: true,
            target_recall: 0.2,
            nprobe: Some(1),
            ..Default::default()
        };
        let response = registry.query(request).await.expect("query");

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

        let mut request = vector_query("ns", vec![1.0, 0.0], 1);
        request.min_wal_sequence = Some(pointer.sequence + 1);
        let err = registry
            .query(request)
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

        let mut request = vector_query("ns", vec![0.0, 1.0], 1);
        request.min_wal_sequence = Some(second_pointer.sequence);
        let response = registry
            .query(request)
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

        let mut request = vector_query("ns", vec![1.0, 25.0], 1);
        request.ann_params = AnnParams {
            use_ivf: false,
            ..Default::default()
        };
        let response = registry.query(request).await.expect("brute-force query");

        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].id, "doc-15");
    }

    #[tokio::test]
    async fn group_by_aggregates_hits_by_attribute() {
        let store = sample_store();
        let registry = NamespaceRegistry::new(store);
        let docs = vec![
            Document {
                id: "doc-1".to_string(),
                vector: Some(vec![1.0, 0.0]),
                attributes: Some(json!({ "category": "news" })),
            },
            Document {
                id: "doc-2".to_string(),
                vector: Some(vec![0.9, 0.1]),
                attributes: Some(json!({ "category": "news" })),
            },
            Document {
                id: "doc-3".to_string(),
                vector: Some(vec![-1.0, 0.0]),
                attributes: Some(json!({ "category": "sports" })),
            },
        ];

        registry
            .apply_write(WriteBatch {
                namespace: "ns".to_string(),
                upserts: docs,
                deletes: Vec::new(),
            })
            .await
            .expect("seed documents");

        let mut request = vector_query("ns", vec![1.0, 0.0], 3);
        request.group_by = Some(GroupBy {
            field: "category".to_string(),
            limit: 5,
            per_group_limit: 2,
        });
        let response = registry.query(request).await.expect("grouped query");

        let groups = response.groups.expect("expected groups");
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].value, "news");
        assert_eq!(groups[0].count, 2);
        assert_eq!(groups[0].hits.len(), 2);
        assert!(groups[0].hits.iter().any(|hit| hit.id == "doc-1"));
        assert!(groups[0].hits.iter().any(|hit| hit.id == "doc-2"));

        assert_eq!(groups[1].value, "sports");
        assert_eq!(groups[1].count, 1);
        assert_eq!(groups[1].hits.len(), 1);
        assert_eq!(groups[1].hits[0].id, "doc-3");
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

    #[tokio::test]
    async fn filters_trim_result_set() {
        let store = sample_store();
        let registry = NamespaceRegistry::new(store);
        let docs = vec![
            Document {
                id: "doc-1".to_string(),
                vector: Some(vec![1.0, 0.0]),
                attributes: Some(json!({ "category": "news" })),
            },
            Document {
                id: "doc-2".to_string(),
                vector: Some(vec![0.0, 1.0]),
                attributes: Some(json!({ "category": "sports" })),
            },
        ];

        registry
            .apply_write(WriteBatch {
                namespace: "ns".to_string(),
                upserts: docs,
                deletes: Vec::new(),
            })
            .await
            .expect("seed docs");

        let mut request = vector_query("ns", vec![1.0, 0.0], 2);
        request.filters = Some(FilterExpr::Eq {
            field: "category".to_string(),
            value: json!("news"),
        });
        let response = registry.query(request).await.expect("filtered query");

        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].id, "doc-1");
    }

    #[tokio::test]
    async fn filter_bitmap_intersection_limits_candidates() {
        let store = sample_store();
        let registry = NamespaceRegistry::new(store);
        let docs = vec![
            Document {
                id: "doc-1".to_string(),
                vector: Some(vec![1.0, 0.0]),
                attributes: Some(json!({ "category": "news" })),
            },
            Document {
                id: "doc-2".to_string(),
                vector: Some(vec![0.0, 1.0]),
                attributes: Some(json!({ "category": "news" })),
            },
            Document {
                id: "doc-3".to_string(),
                vector: Some(vec![0.0, -1.0]),
                attributes: Some(json!({ "category": "sports" })),
            },
        ];

        registry
            .apply_write(WriteBatch {
                namespace: "ns".to_string(),
                upserts: docs,
                deletes: Vec::new(),
            })
            .await
            .expect("seed docs");

        let mut request = vector_query("ns", vec![0.0, 1.0], 3);
        request.filters = Some(FilterExpr::Eq {
            field: "category".to_string(),
            value: json!("news"),
        });
        request.filter_bitmap_ids = Some(vec!["doc-2".to_string()]);
        let response = registry
            .query(request)
            .await
            .expect("bitmap filtered query");

        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].id, "doc-2");
    }

    #[tokio::test]
    async fn bm25_query_returns_textual_matches() {
        let store = sample_store();
        let registry = NamespaceRegistry::new(store);
        let docs = vec![
            Document {
                id: "doc-1".to_string(),
                vector: None,
                attributes: Some(json!({ "content": "Rust search engine overview" })),
            },
            Document {
                id: "doc-2".to_string(),
                vector: None,
                attributes: Some(json!({ "content": "Hybrid search primer" })),
            },
        ];

        registry
            .apply_write(WriteBatch {
                namespace: "ns".to_string(),
                upserts: docs,
                deletes: Vec::new(),
            })
            .await
            .expect("seed bm25 docs");

        let request = QueryRequest {
            namespace: "ns".to_string(),
            top_k: 1,
            rank_by: Some(RankBy::bm25("content", "rust search")),
            queries: Vec::new(),
            metric: None,
            min_wal_sequence: None,
            ann_params: Default::default(),
            group_by: None,
            filters: None,
            filter_bitmap_ids: None,
        };

        let response = registry.query(request).await.expect("bm25 query");
        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].id, "doc-1");
        assert!(response.hits[0].score.is_sign_negative());
    }

    #[tokio::test]
    async fn hybrid_vector_and_bm25_adds_candidates() {
        let store = sample_store();
        let registry = NamespaceRegistry::new(store);
        let docs = vec![
            Document {
                id: "doc-vec".to_string(),
                vector: Some(vec![1.0, 0.0]),
                attributes: Some(json!({ "content": "Rust vector search" })),
            },
            Document {
                id: "doc-text".to_string(),
                vector: None,
                attributes: Some(json!({ "content": "Hybrid search introduction" })),
            },
        ];

        registry
            .apply_write(WriteBatch {
                namespace: "ns".to_string(),
                upserts: docs,
                deletes: Vec::new(),
            })
            .await
            .expect("seed hybrid docs");

        let request = QueryRequest {
            namespace: "ns".to_string(),
            top_k: 2,
            rank_by: Some(RankBy::vector("vector", vec![1.0, 0.0])),
            queries: vec![QueryClause {
                rank_by: RankBy::bm25("content", "hybrid"),
                top_k: Some(1),
            }],
            metric: None,
            min_wal_sequence: None,
            ann_params: Default::default(),
            group_by: None,
            filters: None,
            filter_bitmap_ids: None,
        };

        let response = registry
            .query(request)
            .await
            .expect("hybrid query should succeed");

        let ids: Vec<_> = response.hits.iter().map(|hit| hit.id.as_str()).collect();
        assert!(ids.contains(&"doc-vec"));
        assert!(ids.contains(&"doc-text"));
    }
}
