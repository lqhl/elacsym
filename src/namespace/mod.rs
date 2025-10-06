//! Namespace management
//!
//! Namespace is the core abstraction that ties together:
//! - Manifest (metadata)
//! - Storage (S3/Local)
//! - Vector Index (RaBitQ)
//! - Segments (Parquet files)

pub mod compaction;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::cache::CacheManager;
use crate::index::{FullTextIndex, VectorIndex};
use crate::manifest::{Manifest, ManifestManager};
use crate::query::{fusion, FilterExecutor, FilterExpression, FullTextQuery};
use crate::segment::{SegmentReader, SegmentWriter};
use crate::storage::StorageBackend;
use crate::types::{AttributeValue, DocId, Document, FullTextConfig, Schema, SegmentInfo, Vector};
use crate::wal::{WalManager, WalOperation};
use crate::{Error, Result};

pub use compaction::{CompactionConfig, CompactionManager};

/// Namespace represents a collection of documents with a shared schema
pub struct Namespace {
    name: String,
    node_id: String,  // Node ID for WAL file naming
    storage: Arc<dyn StorageBackend>,
    cache: Option<Arc<CacheManager>>,
    manifest_manager: ManifestManager,

    /// The manifest (protected by RwLock for concurrent access)
    manifest: Arc<RwLock<Manifest>>,

    /// Vector index (protected by RwLock for concurrent writes)
    vector_index: Arc<RwLock<VectorIndex>>,

    /// Full-text indexes (one per full-text field)
    fulltext_indexes: Arc<RwLock<HashMap<String, FullTextIndex>>>,

    /// Write-Ahead Log for durability (protected by RwLock)
    /// Uses local WAL for now, will be replaced with S3WalManager
    wal: Arc<RwLock<WalManager>>,
}

impl Namespace {
    /// Create a new namespace
    pub async fn create(
        name: String,
        schema: Schema,
        storage: Arc<dyn StorageBackend>,
        cache: Option<Arc<CacheManager>>,
        node_id: String,
    ) -> Result<Self> {
        let manifest_manager = ManifestManager::new(storage.clone());

        // Create manifest
        let manifest = manifest_manager
            .create(name.clone(), schema.clone())
            .await?;

        // Create vector index
        let vector_index = VectorIndex::new(schema.vector_dim, schema.vector_metric)?;

        // Create full-text indexes for full_text fields
        let mut fulltext_indexes = HashMap::new();
        for (field_name, attr_schema) in &schema.attributes {
            if attr_schema.full_text.is_enabled() {
                let index = FullTextIndex::new_with_config(
                    field_name.clone(),
                    attr_schema.full_text.clone(),
                )?;
                fulltext_indexes.insert(field_name.clone(), index);
            }
        }

        // Create WAL directory for this namespace
        let wal_dir = format!("wal/{}", name);
        let wal = WalManager::new(&wal_dir).await?;

        Ok(Self {
            name,
            node_id,
            storage,
            cache,
            manifest_manager,
            manifest: Arc::new(RwLock::new(manifest)),
            vector_index: Arc::new(RwLock::new(vector_index)),
            fulltext_indexes: Arc::new(RwLock::new(fulltext_indexes)),
            wal: Arc::new(RwLock::new(wal)),
        })
    }

    /// Load an existing namespace
    pub async fn load(
        name: String,
        storage: Arc<dyn StorageBackend>,
        cache: Option<Arc<CacheManager>>,
        node_id: String,
    ) -> Result<Self> {
        let manifest_manager = ManifestManager::new(storage.clone());

        // Load manifest
        let manifest = manifest_manager.load(&name).await?;

        // Create vector index
        let vector_index =
            VectorIndex::new(manifest.schema.vector_dim, manifest.schema.vector_metric)?;

        // Create full-text indexes for full_text fields
        let mut fulltext_indexes = HashMap::new();
        for (field_name, attr_schema) in &manifest.schema.attributes {
            if attr_schema.full_text.is_enabled() {
                let index = FullTextIndex::new_with_config(
                    field_name.clone(),
                    attr_schema.full_text.clone(),
                )?;
                fulltext_indexes.insert(field_name.clone(), index);
            }
        }

        // Create/Load WAL
        let wal_dir = format!("wal/{}", name);
        let wal = WalManager::new(&wal_dir).await?;

        // Create the namespace instance first
        let namespace = Self {
            name: name.clone(),
            node_id,
            storage,
            cache,
            manifest_manager,
            manifest: Arc::new(RwLock::new(manifest)),
            vector_index: Arc::new(RwLock::new(vector_index)),
            fulltext_indexes: Arc::new(RwLock::new(fulltext_indexes)),
            wal: Arc::new(RwLock::new(wal)),
        };

        // Replay WAL entries if any exist (crash recovery)
        {
            let wal_guard = namespace.wal.read().await;
            let operations = wal_guard.replay().await?;
            drop(wal_guard);

            if !operations.is_empty() {
                tracing::info!(
                    "Replaying {} WAL operations for namespace '{}'",
                    operations.len(),
                    name
                );

                for op in operations {
                    match op {
                        WalOperation::Upsert { documents } => {
                            namespace.upsert_internal(documents).await?;
                        }
                        WalOperation::Delete { .. } => {
                            // TODO: Handle delete operations (future work)
                            tracing::warn!("Delete operations not yet supported in WAL replay");
                        }
                        WalOperation::Commit { .. } => {
                            // Commit markers can be ignored during replay
                        }
                    }
                }

                // Truncate WAL after successful replay
                let mut wal_guard = namespace.wal.write().await;
                wal_guard.truncate().await?;
                tracing::info!("WAL replay complete for namespace '{}'", name);
            }
        }

        // Load existing vectors from segments into indexes
        tracing::info!("Rebuilding indexes for namespace '{}'", name);
        namespace.rebuild_indexes().await?;

        Ok(namespace)
    }

    /// Rebuild vector and full-text indexes from all segments
    ///
    /// NEW APPROACH (per-segment indexes):
    /// - If segment has index_path, load from storage (fast)
    /// - Otherwise, rebuild from segment data (legacy/fallback)
    async fn rebuild_indexes(&self) -> Result<()> {
        let manifest = self.manifest.read().await;
        let segments = manifest.segments.clone();
        let schema = manifest.schema.clone();
        drop(manifest);

        if segments.is_empty() {
            tracing::info!("No segments to rebuild indexes from");
            return Ok(());
        }

        tracing::info!(
            "Rebuilding indexes from {} segments (per-segment mode)",
            segments.len()
        );

        // Strategy: Load per-segment indexes and merge into global in-memory indexes
        // This is a transitional approach - eventually we'll query per-segment indexes directly

        let mut all_vectors: Vec<(DocId, Vector)> = Vec::new();
        let mut all_texts: HashMap<String, Vec<(DocId, String)>> = HashMap::new();

        for segment_info in &segments {
            // Try to load vector index from storage
            if let Some(ref vector_index_path) = segment_info.vector_index_path {
                match VectorIndex::load_from_storage(self.storage.clone(), vector_index_path).await
                {
                    Ok(segment_index) => {
                        // Extract vectors from loaded index
                        tracing::info!(
                            "Loaded vector index from {} ({} vectors)",
                            vector_index_path,
                            segment_index.len()
                        );

                        // Merge vectors into global list
                        for (doc_id, vector) in segment_index
                            .reverse_map
                            .iter()
                            .zip(segment_index.vectors.iter())
                        {
                            all_vectors.push((*doc_id, vector.clone()));
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to load vector index from {}: {}. Falling back to segment rebuild",
                            vector_index_path, e
                        );
                        // Fallback: rebuild from segment data
                        self.rebuild_vector_from_segment(segment_info, &schema, &mut all_vectors)
                            .await?;
                    }
                }
            } else {
                // Legacy segment without index - rebuild from data
                self.rebuild_vector_from_segment(segment_info, &schema, &mut all_vectors)
                    .await?;
            }

            // Try to load full-text indexes from storage
            for (field_name, index_path) in &segment_info.fulltext_index_paths {
                match FullTextIndex::load_from_storage(
                    self.storage.clone(),
                    index_path,
                    field_name.clone(),
                )
                .await
                {
                    Ok(segment_ft_index) => {
                        tracing::info!(
                            "Loaded full-text index for '{}' from {} ({} docs)",
                            field_name,
                            index_path,
                            segment_ft_index.num_docs()
                        );

                        // Note: We can't easily extract texts from Tantivy index
                        // So we still need to read segment data for full-text
                        // This is a limitation - in future we should query per-segment indexes directly
                        self.rebuild_fulltext_from_segment(segment_info, &schema, &mut all_texts)
                            .await?;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to load full-text index from {}: {}. Falling back to segment rebuild",
                            index_path, e
                        );
                        self.rebuild_fulltext_from_segment(segment_info, &schema, &mut all_texts)
                            .await?;
                    }
                }
            }

            // If no full-text index paths, rebuild from segment data
            if segment_info.fulltext_index_paths.is_empty() {
                self.rebuild_fulltext_from_segment(segment_info, &schema, &mut all_texts)
                    .await?;
            }
        }

        // Rebuild global vector index
        if !all_vectors.is_empty() {
            let mut vector_index = self.vector_index.write().await;
            let (ids, vectors): (Vec<_>, Vec<_>) = all_vectors.into_iter().unzip();
            vector_index.add(&ids, &vectors)?;
            tracing::info!("Rebuilt global vector index with {} vectors", ids.len());
        }

        // Rebuild global full-text indexes
        let mut ft_indexes = self.fulltext_indexes.write().await;
        for (field_name, index) in ft_indexes.iter_mut() {
            if let Some(texts) = all_texts.get(field_name) {
                index.add_documents(texts)?;
                tracing::info!(
                    "Rebuilt global full-text index for '{}' with {} docs",
                    field_name,
                    texts.len()
                );
            }
        }

        Ok(())
    }

    /// Helper: rebuild vector index from segment data
    async fn rebuild_vector_from_segment(
        &self,
        segment_info: &SegmentInfo,
        schema: &Schema,
        all_vectors: &mut Vec<(DocId, Vector)>,
    ) -> Result<()> {
        let segment_data = self.storage.get(&segment_info.file_path).await?;
        let writer = SegmentWriter::new(schema.clone())?;
        let reader = SegmentReader::new(writer.arrow_schema);
        let documents = reader.read_parquet(segment_data)?;

        for doc in &documents {
            if let Some(ref vector) = doc.vector {
                all_vectors.push((doc.id, vector.clone()));
            }
        }

        Ok(())
    }

    /// Helper: rebuild full-text indexes from segment data
    async fn rebuild_fulltext_from_segment(
        &self,
        segment_info: &SegmentInfo,
        schema: &Schema,
        all_texts: &mut HashMap<String, Vec<(DocId, String)>>,
    ) -> Result<()> {
        let segment_data = self.storage.get(&segment_info.file_path).await?;
        let writer = SegmentWriter::new(schema.clone())?;
        let reader = SegmentReader::new(writer.arrow_schema);
        let documents = reader.read_parquet(segment_data)?;

        for doc in &documents {
            for (field_name, value) in &doc.attributes {
                if let AttributeValue::String(text) = value {
                    all_texts
                        .entry(field_name.clone())
                        .or_insert_with(Vec::new)
                        .push((doc.id, text.clone()));
                }
            }
        }

        Ok(())
    }

    /// Upsert documents into the namespace
    pub async fn upsert(&self, documents: Vec<Document>) -> Result<usize> {
        if documents.is_empty() {
            return Ok(0);
        }

        // Step 1: Write to WAL first (durability guarantee)
        let mut wal = self.wal.write().await;
        let _wal_seq = wal
            .append(WalOperation::Upsert {
                documents: documents.clone(),
            })
            .await?;
        wal.sync().await?; // Ensure WAL is flushed to disk
        drop(wal);

        // Step 2: Perform actual upsert
        let count = self.upsert_internal(documents).await?;

        // Step 3: Truncate WAL after successful commit
        // All data is now durable in segments + manifest + indexes
        let mut wal = self.wal.write().await;
        wal.truncate().await?;

        Ok(count)
    }

    /// Internal upsert implementation (without WAL write)
    ///
    /// This is used for WAL replay to avoid infinite recursion.
    /// DO NOT call this directly - use upsert() instead.
    async fn upsert_internal(&self, documents: Vec<Document>) -> Result<usize> {
        if documents.is_empty() {
            return Ok(0);
        }

        let manifest = self.manifest.read().await;
        let schema = manifest.schema.clone();
        drop(manifest); // Release read lock

        // Validate documents against schema
        for doc in &documents {
            if let Some(ref vector) = doc.vector {
                if vector.len() != schema.vector_dim {
                    return Err(Error::InvalidSchema(format!(
                        "Vector dimension mismatch: expected {}, got {}",
                        schema.vector_dim,
                        vector.len()
                    )));
                }
            }
        }

        // Write segment to storage
        let segment_id = format!("seg_{}", chrono::Utc::now().timestamp_millis());
        let segment_path = format!("{}/segments/{}.parquet", self.name, segment_id);

        // Create segment writer
        let writer = SegmentWriter::new(schema.clone())?;
        let parquet_data = writer.write_parquet(&documents)?;

        // Upload to storage
        self.storage.put(&segment_path, parquet_data).await?;

        // Calculate ID range
        let ids: Vec<u64> = documents.iter().map(|d| d.id).collect();
        let min_id = *ids.iter().min().unwrap();
        let max_id = *ids.iter().max().unwrap();

        // Build and persist per-segment vector index
        let vector_index_path = {
            let vectors_to_index: Vec<_> = documents
                .iter()
                .filter_map(|doc| doc.vector.as_ref().map(|v| (doc.id, v.clone())))
                .collect();

            if !vectors_to_index.is_empty() {
                let (vec_ids, vectors): (Vec<_>, Vec<_>) = vectors_to_index.into_iter().unzip();

                // Create a new index for this segment
                let mut segment_vector_index =
                    VectorIndex::new(schema.vector_dim, schema.vector_metric)?;
                segment_vector_index.add(&vec_ids, &vectors)?;

                // Persist to storage
                let path = segment_vector_index
                    .build_and_persist(self.storage.clone(), &segment_id, &self.name)
                    .await?;
                Some(path)
            } else {
                None
            }
        };

        // Build and persist per-segment full-text indexes
        let mut fulltext_index_paths = HashMap::new();
        for (field_name, attr_schema) in &schema.attributes {
            if !attr_schema.full_text.is_enabled() {
                continue;
            }

            // Extract texts for this field
            let texts: Vec<(DocId, String)> = documents
                .iter()
                .filter_map(|doc| {
                    doc.attributes.get(field_name).and_then(|v| match v {
                        AttributeValue::String(s) => Some((doc.id, s.clone())),
                        _ => None,
                    })
                })
                .collect();

            if !texts.is_empty() {
                let index_path = FullTextIndex::build_and_persist(
                    field_name.clone(),
                    attr_schema.full_text.clone(),
                    &texts,
                    self.storage.clone(),
                    &segment_id,
                    &self.name,
                )
                .await?;

                fulltext_index_paths.insert(field_name.clone(), index_path);
            }
        }

        // Create segment info with index paths
        let segment_info = SegmentInfo {
            segment_id: segment_id.clone(),
            file_path: segment_path.clone(),
            row_count: documents.len(),
            id_range: (min_id, max_id),
            created_at: chrono::Utc::now(),
            tombstones: vec![],
            vector_index_path,
            fulltext_index_paths,
        };

        // Update manifest
        let mut manifest = self.manifest.write().await;
        manifest.add_segment(segment_info);
        self.manifest_manager.save(&manifest).await?;

        // Update vector index
        let mut index = self.vector_index.write().await;
        let vectors_to_add: Vec<_> = documents
            .iter()
            .filter_map(|doc| doc.vector.as_ref().map(|v| (doc.id, v.clone())))
            .collect();

        if !vectors_to_add.is_empty() {
            let (ids, vectors): (Vec<_>, Vec<_>) = vectors_to_add.into_iter().unzip();
            tracing::debug!(
                "Adding {} vectors to index, current count: {}",
                ids.len(),
                index.vector_count()
            );
            index.add(&ids, &vectors)?;
            tracing::debug!("After add, vector count: {}", index.vector_count());
        }
        drop(index); // Release vector index lock

        // Update full-text indexes
        let mut ft_indexes = self.fulltext_indexes.write().await;
        for (field_name, ft_index) in ft_indexes.iter_mut() {
            // Extract texts for this field
            let texts: Vec<(u64, String)> = documents
                .iter()
                .filter_map(|doc| {
                    doc.attributes.get(field_name).and_then(|v| match v {
                        AttributeValue::String(s) => Some((doc.id, s.clone())),
                        _ => None,
                    })
                })
                .collect();

            if !texts.is_empty() {
                ft_index.add_documents(&texts)?;
            }
        }

        Ok(documents.len())
    }

    /// Query the namespace with full document retrieval
    pub async fn query(
        &self,
        query_vector: Option<&[f32]>,
        full_text_query: Option<&FullTextQuery>,
        top_k: usize,
        filter: Option<&FilterExpression>,
    ) -> Result<Vec<(Document, f32)>> {
        // Step 1: Apply filter if present
        let filtered_ids = if let Some(filter_expr) = filter {
            let manifest = self.manifest.read().await;
            let segments = manifest.segments.clone();
            let schema = manifest.schema.clone();
            drop(manifest);

            Some(
                FilterExecutor::apply_filter(&segments, filter_expr, &schema, &*self.storage)
                    .await?,
            )
        } else {
            None
        };

        // Step 2: Execute vector search (if requested)
        let vector_results = if let Some(vector) = query_vector {
            let mut index = self.vector_index.write().await;
            let query_vec = vector.to_vec();

            tracing::debug!(
                "Vector search: dimension={}, top_k={}, vector_count={}",
                query_vec.len(),
                top_k,
                index.vector_count()
            );

            let results = index.search(&query_vec, top_k * 2)?; // Over-sample for merging

            tracing::debug!("Vector search returned {} results", results.len());

            drop(index);
            Some(results)
        } else {
            None
        };

        // Step 3: Execute full-text search (if requested)
        let fulltext_results = if let Some(ft_query) = full_text_query {
            let ft_indexes = self.fulltext_indexes.read().await;

            // Get all fields involved in the query
            let fields = ft_query.fields();
            let query_text = ft_query.query_text();

            // Collect results from all fields
            let mut field_results: Vec<Vec<(u64, f32)>> = Vec::new();

            for field in &fields {
                if let Some(index) = ft_indexes.get(*field) {
                    let results = index.search(query_text, top_k * 2)?; // Over-sample

                    // Apply field weight
                    let weight = ft_query.field_weight(field);
                    let weighted_results: Vec<(u64, f32)> = results
                        .into_iter()
                        .map(|(id, score)| (id, score * weight))
                        .collect();

                    field_results.push(weighted_results);
                } else {
                    return Err(Error::InvalidRequest(format!(
                        "Field '{}' is not configured for full-text search",
                        field
                    )));
                }
            }

            // Combine multi-field results
            if !field_results.is_empty() {
                Some(Self::combine_field_results(field_results, top_k))
            } else {
                None
            }
        } else {
            None
        };

        // Step 4: Merge results (simple union for Phase 1, RRF in Phase 2)
        let mut combined_results =
            Self::merge_search_results(vector_results, fulltext_results, top_k);

        // Step 5: Filter search results if we have a filter
        if let Some(ref allowed_ids) = filtered_ids {
            combined_results.retain(|(id, _)| allowed_ids.contains(id));
        }

        if combined_results.is_empty() {
            return Ok(vec![]);
        }

        // Step 6: Group doc IDs by segment
        let doc_ids: Vec<u64> = combined_results.iter().map(|(id, _)| *id).collect();
        let manifest = self.manifest.read().await;
        let segments = manifest.segments.clone();
        drop(manifest);

        // Create a map: segment_id -> doc_ids in that segment
        let mut segment_to_docs: HashMap<String, Vec<u64>> = HashMap::new();

        for doc_id in &doc_ids {
            // Find which segment contains this doc_id
            for segment in &segments {
                if doc_id >= &segment.id_range.0 && doc_id <= &segment.id_range.1 {
                    segment_to_docs
                        .entry(segment.segment_id.clone())
                        .or_insert_with(Vec::new)
                        .push(*doc_id);
                    break;
                }
            }
        }

        // Step 7: Fetch documents from segments (with caching)
        let mut all_documents: HashMap<u64, Document> = HashMap::new();

        for (segment_id, ids_in_segment) in segment_to_docs {
            // Find segment info
            let segment_info = segments
                .iter()
                .find(|s| s.segment_id == segment_id)
                .ok_or_else(|| Error::internal(format!("Segment {} not found", segment_id)))?;

            // Load segment data from storage (with cache)
            let segment_data = if let Some(ref cache) = self.cache {
                let cache_key = CacheManager::segment_key(&self.name, &segment_id);
                let storage = self.storage.clone();
                let path = segment_info.file_path.clone();

                cache
                    .get_or_fetch(&cache_key, || async move { storage.get(&path).await })
                    .await?
            } else {
                // No cache - fetch directly
                self.storage.get(&segment_info.file_path).await?
            };

            // Read documents
            let manifest_guard = self.manifest.read().await;
            let arrow_schema = SegmentWriter::new(manifest_guard.schema.clone())?.arrow_schema;
            drop(manifest_guard);

            let reader = SegmentReader::new(arrow_schema);
            let documents = reader.read_documents_by_ids(segment_data, &ids_in_segment)?;

            // Store in map
            for doc in documents {
                all_documents.insert(doc.id, doc);
            }
        }

        // Step 8: Assemble results in order
        let mut results = Vec::with_capacity(combined_results.len());
        for (doc_id, score) in combined_results {
            if let Some(document) = all_documents.remove(&doc_id) {
                results.push((document, score));
            }
        }

        Ok(results)
    }

    /// Combine results from multiple full-text fields
    ///
    /// Sum scores from all fields for each document ID
    fn combine_field_results(field_results: Vec<Vec<(u64, f32)>>, top_k: usize) -> Vec<(u64, f32)> {
        let mut combined: HashMap<u64, f32> = HashMap::new();

        // Sum scores from all fields
        for results in field_results {
            for (id, score) in results {
                *combined.entry(id).or_insert(0.0) += score;
            }
        }

        // Sort by score and take top_k
        let mut results: Vec<_> = combined.into_iter().collect();
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(top_k * 2); // Keep 2x for later merging
        results
    }

    /// Get the namespace schema
    pub async fn schema(&self) -> Schema {
        let manifest = self.manifest.read().await;
        manifest.schema.clone()
    }

    /// Get namespace statistics
    pub async fn stats(&self) -> crate::types::NamespaceStats {
        let manifest = self.manifest.read().await;
        manifest.stats.clone()
    }

    /// Get the number of segments
    pub async fn segment_count(&self) -> usize {
        let manifest = self.manifest.read().await;
        manifest.segments.len()
    }

    /// Merge vector and full-text search results using RRF
    ///
    /// Uses Reciprocal Rank Fusion algorithm to combine results from
    /// vector search and full-text search.
    ///
    /// # Arguments
    /// * `vector_results` - Ranked list from vector search
    /// * `fulltext_results` - Ranked list from full-text search
    /// * `top_k` - Number of results to return
    ///
    /// # Returns
    /// Combined results sorted by RRF score
    fn merge_search_results(
        vector_results: Option<Vec<(u64, f32)>>,
        fulltext_results: Option<Vec<(u64, f32)>>,
        top_k: usize,
    ) -> Vec<(u64, f32)> {
        // Use RRF fusion with equal weights (0.5, 0.5)
        fusion::reciprocal_rank_fusion(
            vector_results.as_deref(),
            fulltext_results.as_deref(),
            0.5,  // vector weight
            0.5,  // fulltext weight
            60.0, // RRF constant k
            top_k,
        )
    }

    /// Check if compaction is needed (using default config)
    ///
    /// Compaction is triggered when:
    /// - Number of segments > 100, OR
    /// - Total segment size > 1GB
    pub async fn should_compact(&self) -> bool {
        let config = CompactionConfig::default();
        self.should_compact_with_config(&config).await
    }

    /// Check if compaction is needed with custom config
    pub async fn should_compact_with_config(&self, config: &CompactionConfig) -> bool {
        let manifest = self.manifest.read().await;

        // Trigger if too many segments
        if manifest.segments.len() > config.max_segments {
            return true;
        }

        // Trigger if total size exceeds threshold
        let total_docs: usize = manifest.segments.iter().map(|s| s.row_count).sum();
        total_docs > config.max_total_docs
    }

    /// Perform compaction to merge small segments
    ///
    /// This implements LSM-tree style compaction:
    /// 1. Select smallest N segments to merge
    /// 2. Read all documents from selected segments
    /// 3. Write merged data to new segment
    /// 4. Rebuild vector and full-text indexes
    /// 5. Atomically update manifest
    /// 6. Delete old segment files
    pub async fn compact(&self) -> Result<()> {
        tracing::info!("Starting compaction for namespace: {}", self.name);

        // Step 1: Select segments to merge (smallest 10 segments)
        let segments_to_merge = {
            let manifest = self.manifest.read().await;

            if manifest.segments.len() < 2 {
                tracing::info!("Not enough segments to compact");
                return Ok(());
            }

            // Sort by size and select smallest segments (up to 10)
            let mut sorted_segments = manifest.segments.clone();
            sorted_segments.sort_by_key(|s| s.row_count);
            sorted_segments.truncate(10);
            sorted_segments
        };

        tracing::info!("Merging {} segments", segments_to_merge.len());

        // Step 2: Read all documents from selected segments
        let mut all_documents = Vec::new();
        for segment_info in &segments_to_merge {
            let data = self.storage.get(&segment_info.file_path).await?;

            let schema = self.manifest.read().await.schema.clone();
            let reader = SegmentReader::new(SegmentWriter::new(schema)?.arrow_schema);
            let docs = reader.read_parquet(data)?;
            all_documents.extend(docs);
        }

        tracing::info!("Read {} documents from segments", all_documents.len());

        // Step 3: Write merged data to new segment
        let new_segment_id = format!("seg_{}", uuid::Uuid::new_v4());
        let segment_path = format!("{}/segments/{}.parquet", self.name, new_segment_id);

        let schema = self.manifest.read().await.schema.clone();
        let writer = SegmentWriter::new(schema)?;
        let parquet_data = writer.write_parquet(&all_documents)?;

        // Write to storage
        self.storage.put(&segment_path, parquet_data).await?;

        // Calculate ID range
        let ids: Vec<u64> = all_documents.iter().map(|d| d.id).collect();
        let min_id = *ids.iter().min().unwrap();
        let max_id = *ids.iter().max().unwrap();

        tracing::info!("Wrote merged segment: {} ({} docs)", new_segment_id, all_documents.len());

        // Build per-segment indexes (same as upsert_internal)
        let schema = self.manifest.read().await.schema.clone();

        // Build vector index
        let vector_index_path = {
            let vectors_to_index: Vec<_> = all_documents
                .iter()
                .filter_map(|doc| doc.vector.as_ref().map(|v| (doc.id, v.clone())))
                .collect();

            if !vectors_to_index.is_empty() {
                let (vec_ids, vectors): (Vec<_>, Vec<_>) = vectors_to_index.into_iter().unzip();
                let mut segment_vector_index =
                    VectorIndex::new(schema.vector_dim, schema.vector_metric)?;
                segment_vector_index.add(&vec_ids, &vectors)?;

                let path = segment_vector_index
                    .build_and_persist(self.storage.clone(), &new_segment_id, &self.name)
                    .await?;
                Some(path)
            } else {
                None
            }
        };

        // Build full-text indexes
        let mut fulltext_index_paths = HashMap::new();
        for (field_name, attr_schema) in &schema.attributes {
            if !attr_schema.full_text.is_enabled() {
                continue;
            }

            let texts: Vec<(DocId, String)> = all_documents
                .iter()
                .filter_map(|doc| {
                    doc.attributes.get(field_name).and_then(|v| match v {
                        AttributeValue::String(s) => Some((doc.id, s.clone())),
                        _ => None,
                    })
                })
                .collect();

            if !texts.is_empty() {
                let index_path = FullTextIndex::build_and_persist(
                    field_name.clone(),
                    attr_schema.full_text.clone(),
                    &texts,
                    self.storage.clone(),
                    &new_segment_id,
                    &self.name,
                )
                .await?;

                fulltext_index_paths.insert(field_name.clone(), index_path);
            }
        }

        // Create segment info with index paths
        let segment_info = SegmentInfo {
            segment_id: new_segment_id.clone(),
            file_path: segment_path.clone(),
            row_count: all_documents.len(),
            id_range: (min_id, max_id),
            created_at: chrono::Utc::now(),
            tombstones: Vec::new(),
            vector_index_path,
            fulltext_index_paths,
        };

        tracing::info!("Built per-segment indexes for merged segment");

        // Step 4: Rebuild indexes with merged segment data only
        // Note: We only rebuild with documents from merged segments, not all documents
        {
            let manifest = self.manifest.read().await;

            // Rebuild vector index
            let mut vector_index = self.vector_index.write().await;

            // Collect vectors and IDs
            let mut ids = Vec::new();
            let mut vectors = Vec::new();
            for doc in &all_documents {
                if let Some(vector) = &doc.vector {
                    ids.push(doc.id);
                    vectors.push(vector.clone());
                }
            }

            // Add to index (this will trigger rebuild internally)
            if !ids.is_empty() {
                vector_index.add(&ids, &vectors)?;
                tracing::info!("Rebuilt vector index with {} vectors", ids.len());
            }

            // Rebuild full-text indexes
            let mut fulltext_indexes = self.fulltext_indexes.write().await;
            for (field_name, index) in fulltext_indexes.iter_mut() {
                let mut field_docs = Vec::new();
                for doc in &all_documents {
                    if let Some(AttributeValue::String(text)) = doc.attributes.get(field_name) {
                        field_docs.push((doc.id, text.clone()));
                    }
                }

                if !field_docs.is_empty() {
                    // Recreate index with all documents
                    // Note: This is inefficient, but Tantivy doesn't support bulk delete
                    // In production, we'd create a new index and swap
                    *index = FullTextIndex::new_with_config(
                        field_name.clone(),
                        manifest
                            .schema
                            .attributes
                            .get(field_name)
                            .map(|a| a.full_text.clone())
                            .unwrap_or(FullTextConfig::Simple(false)),
                    )?;
                    index.add_documents(&field_docs)?;

                    tracing::info!(
                        "Rebuilt full-text index for field '{}' with {} docs",
                        field_name,
                        field_docs.len()
                    );
                }
            }
        }

        // Step 5: Atomically update manifest
        {
            let mut manifest = self.manifest.write().await;

            // Remove old segments
            let old_segment_ids: HashSet<_> = segments_to_merge
                .iter()
                .map(|s| s.segment_id.clone())
                .collect();
            manifest
                .segments
                .retain(|s| !old_segment_ids.contains(&s.segment_id));

            // Add new segment
            manifest.segments.push(segment_info);
            manifest.version += 1;

            // Update stats
            let total_docs: usize = manifest.segments.iter().map(|s| s.row_count).sum();
            manifest.stats.total_docs = total_docs;

            // Save manifest
            let manifest_path = format!("{}/manifest.json", self.name);
            let manifest_json = serde_json::to_vec_pretty(&*manifest)
                .map_err(|e| Error::internal(format!("Failed to serialize manifest: {}", e)))?;
            self.storage
                .put(&manifest_path, manifest_json.into())
                .await?;
        }

        tracing::info!("Updated manifest after compaction");

        // Step 6: Delete old segment files
        for segment_info in &segments_to_merge {
            let path = format!("{}/{}", self.name, segment_info.file_path);
            if let Err(e) = self.storage.delete(&path).await {
                tracing::warn!("Failed to delete old segment {}: {}", path, e);
                // Don't fail compaction if cleanup fails
            }
        }

        tracing::info!("Compaction completed successfully");
        Ok(())
    }
}

/// NamespaceManager manages multiple namespaces
pub struct NamespaceManager {
    storage: Arc<dyn StorageBackend>,
    cache: Option<Arc<CacheManager>>,
    namespaces: Arc<RwLock<HashMap<String, Arc<Namespace>>>>,
    compaction_config: CompactionConfig,
    compaction_managers: Arc<RwLock<HashMap<String, Arc<CompactionManager>>>>,
    node_id: String,  // Node ID for this manager
}

impl NamespaceManager {
    pub fn new(storage: Arc<dyn StorageBackend>, node_id: String) -> Self {
        Self {
            storage,
            cache: None,
            namespaces: Arc::new(RwLock::new(HashMap::new())),
            compaction_config: CompactionConfig::default(),
            compaction_managers: Arc::new(RwLock::new(HashMap::new())),
            node_id,
        }
    }

    /// Create a new NamespaceManager with cache
    pub fn with_cache(
        storage: Arc<dyn StorageBackend>,
        cache: Arc<CacheManager>,
        node_id: String,
    ) -> Self {
        Self {
            storage,
            cache: Some(cache),
            namespaces: Arc::new(RwLock::new(HashMap::new())),
            compaction_config: CompactionConfig::default(),
            compaction_managers: Arc::new(RwLock::new(HashMap::new())),
            node_id,
        }
    }

    /// Create a new NamespaceManager with custom compaction config
    pub fn with_compaction_config(
        storage: Arc<dyn StorageBackend>,
        cache: Option<Arc<CacheManager>>,
        compaction_config: CompactionConfig,
        node_id: String,
    ) -> Self {
        Self {
            storage,
            cache,
            namespaces: Arc::new(RwLock::new(HashMap::new())),
            compaction_config,
            compaction_managers: Arc::new(RwLock::new(HashMap::new())),
            node_id,
        }
    }

    /// Get the node ID
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Create a new namespace
    pub async fn create_namespace(&self, name: String, schema: Schema) -> Result<Arc<Namespace>> {
        // Check if namespace already exists
        {
            let namespaces = self.namespaces.read().await;
            if namespaces.contains_key(&name) {
                return Err(Error::InvalidRequest(format!(
                    "Namespace '{}' already exists",
                    name
                )));
            }
        }

        // Create namespace
        let namespace = Arc::new(
            Namespace::create(
                name.clone(),
                schema,
                self.storage.clone(),
                self.cache.clone(),
                self.node_id.clone(),
            )
            .await?,
        );

        // Start compaction manager for this namespace
        let compaction_manager = Arc::new(CompactionManager::new(self.compaction_config.clone()));
        compaction_manager
            .start_for_namespace(namespace.clone())
            .await?;

        // Store in caches
        {
            let mut namespaces = self.namespaces.write().await;
            namespaces.insert(name.clone(), namespace.clone());
        }
        {
            let mut managers = self.compaction_managers.write().await;
            managers.insert(name, compaction_manager);
        }

        Ok(namespace)
    }

    /// Get or load a namespace
    pub async fn get_namespace(&self, name: &str) -> Result<Arc<Namespace>> {
        // Check cache first
        {
            let namespaces = self.namespaces.read().await;
            if let Some(ns) = namespaces.get(name) {
                return Ok(ns.clone());
            }
        }

        // Try to load from storage
        let namespace = Arc::new(
            Namespace::load(
                name.to_string(),
                self.storage.clone(),
                self.cache.clone(),
                self.node_id.clone(),
            )
            .await?,
        );

        // Start compaction manager for this namespace (if not already started)
        let compaction_manager = Arc::new(CompactionManager::new(self.compaction_config.clone()));
        compaction_manager
            .start_for_namespace(namespace.clone())
            .await?;

        // Store in caches
        {
            let mut namespaces = self.namespaces.write().await;
            namespaces.insert(name.to_string(), namespace.clone());
        }
        {
            let mut managers = self.compaction_managers.write().await;
            managers.insert(name.to_string(), compaction_manager);
        }

        Ok(namespace)
    }

    /// List all namespaces
    pub async fn list_namespaces(&self) -> Result<Vec<String>> {
        // List all manifest files in storage
        let keys = self.storage.list("").await?;

        let namespaces: Vec<String> = keys
            .iter()
            .filter_map(|key| {
                // Extract namespace from path like "namespace/manifest.json"
                if key.ends_with("/manifest.json") {
                    key.strip_suffix("/manifest.json").map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect();

        Ok(namespaces)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::local::LocalStorage;
    use crate::types::{
        AttributeSchema, AttributeType, AttributeValue, DistanceMetric, FullTextConfig,
    };
    use std::collections::HashMap;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_namespace_create_and_upsert() {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());

        let mut attributes = HashMap::new();
        attributes.insert(
            "title".to_string(),
            AttributeSchema {
                attr_type: AttributeType::String,
                indexed: false,
                full_text: FullTextConfig::Simple(true),
            },
        );

        let schema = Schema {
            vector_dim: 128,
            vector_metric: DistanceMetric::L2,
            attributes,
        };

        // Create namespace
        let ns = Namespace::create("test_ns".to_string(), schema, storage.clone(), None, "test-node".to_string())
            .await
            .unwrap();

        // Create documents
        let mut doc1_attrs = HashMap::new();
        doc1_attrs.insert(
            "title".to_string(),
            AttributeValue::String("Test Document 1".to_string()),
        );

        let doc1 = Document {
            id: 1,
            vector: Some(vec![1.0; 128]),
            attributes: doc1_attrs,
        };

        let mut doc2_attrs = HashMap::new();
        doc2_attrs.insert(
            "title".to_string(),
            AttributeValue::String("Test Document 2".to_string()),
        );

        let doc2 = Document {
            id: 2,
            vector: Some(vec![2.0; 128]),
            attributes: doc2_attrs,
        };

        // Upsert documents
        let count = ns.upsert(vec![doc1, doc2]).await.unwrap();
        assert_eq!(count, 2);

        // Check stats
        let stats = ns.stats().await;
        assert_eq!(stats.total_docs, 2);
        assert_eq!(stats.segment_count, 1);
    }

    #[tokio::test]
    async fn test_namespace_query() {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());

        let mut attributes = HashMap::new();
        attributes.insert(
            "title".to_string(),
            AttributeSchema {
                attr_type: AttributeType::String,
                indexed: false,
                full_text: FullTextConfig::Simple(false),
            },
        );

        let schema = Schema {
            vector_dim: 64,
            vector_metric: DistanceMetric::L2,
            attributes,
        };

        let ns = Namespace::create("test_ns".to_string(), schema, storage, None, "test-node".to_string())
            .await
            .unwrap();

        // Add some documents
        let mut attrs1 = HashMap::new();
        attrs1.insert(
            "title".to_string(),
            AttributeValue::String("Doc 1".to_string()),
        );

        let mut attrs2 = HashMap::new();
        attrs2.insert(
            "title".to_string(),
            AttributeValue::String("Doc 2".to_string()),
        );

        let mut attrs3 = HashMap::new();
        attrs3.insert(
            "title".to_string(),
            AttributeValue::String("Doc 3".to_string()),
        );

        let docs = vec![
            Document {
                id: 1,
                vector: Some(vec![1.0; 64]),
                attributes: attrs1,
            },
            Document {
                id: 2,
                vector: Some(vec![2.0; 64]),
                attributes: attrs2,
            },
            Document {
                id: 3,
                vector: Some(vec![3.0; 64]),
                attributes: attrs3,
            },
        ];

        ns.upsert(docs).await.unwrap();

        // Query
        let query = vec![2.5; 64];
        let results = ns.query(Some(&query), None, 2, None).await.unwrap();

        assert_eq!(results.len(), 2);
        // Should return closest vectors with documents
        assert!(results.iter().any(|(doc, _)| doc.id == 2 || doc.id == 3));

        // Verify documents have attributes
        for (doc, _distance) in &results {
            assert!(doc.attributes.contains_key("title"));
        }
    }

    #[tokio::test]
    async fn test_namespace_query_with_filter() {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());

        let mut attributes = HashMap::new();
        attributes.insert(
            "title".to_string(),
            AttributeSchema {
                attr_type: AttributeType::String,
                indexed: false,
                full_text: FullTextConfig::Simple(false),
            },
        );
        attributes.insert(
            "category".to_string(),
            AttributeSchema {
                attr_type: AttributeType::String,
                indexed: true,
                full_text: FullTextConfig::Simple(false),
            },
        );
        attributes.insert(
            "score".to_string(),
            AttributeSchema {
                attr_type: AttributeType::Float,
                indexed: false,
                full_text: FullTextConfig::Simple(false),
            },
        );

        let schema = Schema {
            vector_dim: 64,
            vector_metric: DistanceMetric::L2,
            attributes,
        };

        let ns = Namespace::create("test_ns".to_string(), schema, storage, None, "test-node".to_string())
            .await
            .unwrap();

        // Add documents with different categories
        let mut attrs1 = HashMap::new();
        attrs1.insert(
            "title".to_string(),
            AttributeValue::String("Tech Doc 1".to_string()),
        );
        attrs1.insert(
            "category".to_string(),
            AttributeValue::String("tech".to_string()),
        );
        attrs1.insert("score".to_string(), AttributeValue::Float(4.5));

        let mut attrs2 = HashMap::new();
        attrs2.insert(
            "title".to_string(),
            AttributeValue::String("Sports Doc 2".to_string()),
        );
        attrs2.insert(
            "category".to_string(),
            AttributeValue::String("sports".to_string()),
        );
        attrs2.insert("score".to_string(), AttributeValue::Float(3.5));

        let mut attrs3 = HashMap::new();
        attrs3.insert(
            "title".to_string(),
            AttributeValue::String("Tech Doc 3".to_string()),
        );
        attrs3.insert(
            "category".to_string(),
            AttributeValue::String("tech".to_string()),
        );
        attrs3.insert("score".to_string(), AttributeValue::Float(4.8));

        let docs = vec![
            Document {
                id: 1,
                vector: Some(vec![1.0; 64]),
                attributes: attrs1,
            },
            Document {
                id: 2,
                vector: Some(vec![2.0; 64]),
                attributes: attrs2,
            },
            Document {
                id: 3,
                vector: Some(vec![3.0; 64]),
                attributes: attrs3,
            },
        ];

        ns.upsert(docs).await.unwrap();

        // Query with filter: category = "tech" AND score >= 4.0
        let filter = crate::query::FilterExpression::And {
            conditions: vec![
                crate::query::FilterCondition {
                    field: "category".to_string(),
                    op: crate::query::FilterOp::Eq,
                    value: AttributeValue::String("tech".to_string()),
                },
                crate::query::FilterCondition {
                    field: "score".to_string(),
                    op: crate::query::FilterOp::Gte,
                    value: AttributeValue::Float(4.0),
                },
            ],
        };

        let query = vec![2.0; 64];
        let results = ns
            .query(Some(&query), None, 10, Some(&filter))
            .await
            .unwrap();

        // Should only return doc 1 and 3 (both tech with score >= 4.0)
        assert_eq!(results.len(), 2);
        for (doc, _) in &results {
            let category = doc.attributes.get("category").unwrap();
            assert_eq!(category, &AttributeValue::String("tech".to_string()));

            let score = doc.attributes.get("score").unwrap();
            match score {
                AttributeValue::Float(s) => assert!(*s >= 4.0),
                _ => panic!("Expected float score"),
            }
        }
    }

    #[tokio::test]
    async fn test_namespace_fulltext_search() {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());

        let mut attributes = HashMap::new();
        attributes.insert(
            "title".to_string(),
            AttributeSchema {
                attr_type: AttributeType::String,
                indexed: false,
                full_text: FullTextConfig::Simple(true), // Enable full-text search
            },
        );

        let schema = Schema {
            vector_dim: 64,
            vector_metric: DistanceMetric::L2,
            attributes,
        };

        let ns = Namespace::create("test_ns".to_string(), schema, storage, None, "test-node".to_string())
            .await
            .unwrap();

        // Add documents with different titles
        let mut attrs1 = HashMap::new();
        attrs1.insert(
            "title".to_string(),
            AttributeValue::String("Rust programming language".to_string()),
        );

        let mut attrs2 = HashMap::new();
        attrs2.insert(
            "title".to_string(),
            AttributeValue::String("Rust vector database".to_string()),
        );

        let mut attrs3 = HashMap::new();
        attrs3.insert(
            "title".to_string(),
            AttributeValue::String("Python programming tutorial".to_string()),
        );

        let docs = vec![
            Document {
                id: 1,
                vector: Some(vec![1.0; 64]),
                attributes: attrs1,
            },
            Document {
                id: 2,
                vector: Some(vec![2.0; 64]),
                attributes: attrs2,
            },
            Document {
                id: 3,
                vector: Some(vec![3.0; 64]),
                attributes: attrs3,
            },
        ];

        ns.upsert(docs).await.unwrap();

        // Full-text search for "rust"
        let ft_query = crate::query::FullTextQuery::Single {
            field: "title".to_string(),
            query: "rust".to_string(),
            weight: 1.0,
        };

        let results = ns.query(None, Some(&ft_query), 10, None).await.unwrap();

        // Should return docs 1 and 2 (both contain "rust")
        assert_eq!(results.len(), 2);
        let ids: Vec<u64> = results.iter().map(|(doc, _)| doc.id).collect();
        assert!(ids.contains(&1));
        assert!(ids.contains(&2));
        assert!(!ids.contains(&3));
    }

    #[tokio::test]
    async fn test_namespace_hybrid_search() {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());

        let mut attributes = HashMap::new();
        attributes.insert(
            "content".to_string(),
            AttributeSchema {
                attr_type: AttributeType::String,
                indexed: false,
                full_text: FullTextConfig::Simple(true),
            },
        );

        let schema = Schema {
            vector_dim: 64,
            vector_metric: DistanceMetric::L2, // RaBitQ only supports L2
            attributes,
        };

        let ns = Namespace::create("test_ns".to_string(), schema, storage, None, "test-node".to_string())
            .await
            .unwrap();

        // Add documents
        let mut attrs1 = HashMap::new();
        attrs1.insert(
            "content".to_string(),
            AttributeValue::String("machine learning algorithms".to_string()),
        );

        let mut attrs2 = HashMap::new();
        attrs2.insert(
            "content".to_string(),
            AttributeValue::String("deep learning neural networks".to_string()),
        );

        let mut attrs3 = HashMap::new();
        attrs3.insert(
            "content".to_string(),
            AttributeValue::String("database systems".to_string()),
        );

        let docs = vec![
            Document {
                id: 1,
                vector: Some(vec![1.0; 64]),
                attributes: attrs1,
            },
            Document {
                id: 2,
                vector: Some(vec![0.9; 64]),
                attributes: attrs2,
            },
            Document {
                id: 3,
                vector: Some(vec![5.0; 64]),
                attributes: attrs3,
            },
        ];

        ns.upsert(docs).await.unwrap();

        // Hybrid search: vector + full-text
        let query_vector = vec![1.0; 64];
        let ft_query = crate::query::FullTextQuery::Single {
            field: "content".to_string(),
            query: "learning".to_string(),
            weight: 0.5,
        };

        let results = ns
            .query(Some(&query_vector), Some(&ft_query), 10, None)
            .await
            .unwrap();

        // Should return docs that match vector similarity OR text relevance
        assert!(results.len() >= 2);

        // Docs 1 and 2 should be in results (similar vectors + contain "learning")
        let ids: Vec<u64> = results.iter().map(|(doc, _)| doc.id).collect();
        assert!(ids.contains(&1) || ids.contains(&2));
    }
}
