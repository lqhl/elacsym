#![recursion_limit = "4096"]

//! Storage layer abstractions for WAL, parts, and router metadata.

use std::{
    collections::BTreeMap,
    fmt,
    io::ErrorKind,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use arrow_array::builder::{Float32Builder, LargeStringBuilder, ListBuilder, StringBuilder};
use arrow_array::{
    Array, ArrayRef, Float32Array, LargeStringArray, ListArray, RecordBatch, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use bytes::Bytes;
use elax_filter::FilterExpr;
use futures::TryStreamExt;
use object_store::{path::Path as ObjectPath, Error as ObjectStoreError, ObjectStore};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::arrow_writer::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{fs, io::AsyncWriteExt};

/// Root handle for interacting with namespace storage on the local filesystem.
#[derive(Clone, Debug)]
pub struct LocalStore {
    root: PathBuf,
    fsync: bool,
    object_store: Option<ObjectStoreRoot>,
}

impl LocalStore {
    /// Create a new store rooted at `root`. Directories are created lazily.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            fsync: true,
            object_store: None,
        }
    }

    /// Configure whether WAL writes call `File::sync_all` after append.
    pub fn with_fsync(mut self, fsync: bool) -> Self {
        self.fsync = fsync;
        self
    }

    /// Attach an object-store client used for part asset materialization.
    pub fn with_object_store(
        mut self,
        store: Arc<dyn ObjectStore>,
        prefix: impl Into<String>,
    ) -> Self {
        self.object_store = Some(ObjectStoreRoot::new(store, prefix.into()));
        self
    }

    /// Return a namespace-scoped handle.
    pub fn namespace(&self, namespace: impl Into<String>) -> NamespaceStore {
        let namespace = namespace.into();
        let object_store = self
            .object_store
            .as_ref()
            .map(|root| root.for_namespace(&namespace));
        NamespaceStore {
            root: self.root.clone(),
            namespace,
            fsync: self.fsync,
            object_store,
        }
    }
}

/// Namespace-scoped store that manages WAL and router state.
#[derive(Clone, Debug)]
pub struct NamespaceStore {
    root: PathBuf,
    namespace: String,
    fsync: bool,
    object_store: Option<NamespaceObjectStore>,
}

#[derive(Clone)]
struct ObjectStoreRoot {
    client: Arc<dyn ObjectStore>,
    prefix: String,
}

impl ObjectStoreRoot {
    fn new(client: Arc<dyn ObjectStore>, prefix: String) -> Self {
        Self {
            client,
            prefix: normalize_prefix(&prefix),
        }
    }

    fn for_namespace(&self, namespace: &str) -> NamespaceObjectStore {
        let mut components = Vec::new();
        if !self.prefix.is_empty() {
            components.push(self.prefix.as_str());
        }
        components.extend(["namespaces", namespace]);
        let prefix = components.join("/");
        NamespaceObjectStore {
            client: self.client.clone(),
            prefix,
        }
    }
}

impl fmt::Debug for ObjectStoreRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ObjectStoreRoot")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct NamespaceObjectStore {
    client: Arc<dyn ObjectStore>,
    prefix: String,
}

impl NamespaceObjectStore {
    fn join(&self, components: &[&str]) -> ObjectPath {
        let mut parts: Vec<&str> = Vec::with_capacity(components.len() + 1);
        if !self.prefix.is_empty() {
            parts.push(self.prefix.as_str());
        }
        parts.extend_from_slice(components);
        ObjectPath::from(parts.join("/"))
    }

    fn part_path(&self, part_id: &str, components: &[&str]) -> ObjectPath {
        let mut parts: Vec<&str> = Vec::with_capacity(components.len() + 2);
        parts.push("parts");
        parts.push(part_id);
        parts.extend_from_slice(components);
        self.join(&parts)
    }

    fn part_prefix(&self, part_id: &str) -> ObjectPath {
        self.join(&["parts", part_id])
    }

    async fn put(&self, path: ObjectPath, bytes: Bytes) -> Result<()> {
        self.client
            .put(&path, bytes)
            .await
            .with_context(|| format!("writing object {path}"))
    }

    async fn delete_if_exists(&self, path: ObjectPath) -> Result<()> {
        match self.client.delete(&path).await {
            Ok(_) => Ok(()),
            Err(ObjectStoreError::NotFound { .. }) => Ok(()),
            Err(err) => Err(err).with_context(|| format!("deleting object {path}")),
        }
    }

    async fn delete_prefix(&self, prefix: ObjectPath) -> Result<()> {
        let mut entries = self
            .client
            .list(Some(&prefix))
            .await
            .with_context(|| format!("listing objects under {prefix}"))?;
        while let Some(meta) = entries.try_next().await? {
            self.delete_if_exists(meta.location).await?;
        }
        Ok(())
    }

    async fn get(&self, path: ObjectPath) -> Result<Option<Bytes>> {
        match self.client.get(&path).await {
            Ok(result) => {
                let bytes = result
                    .bytes()
                    .await
                    .with_context(|| format!("reading object {path}"))?;
                Ok(Some(bytes))
            }
            Err(ObjectStoreError::NotFound { .. }) => Ok(None),
            Err(err) => Err(err).with_context(|| format!("fetching object {path}")),
        }
    }
}

impl fmt::Debug for NamespaceObjectStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NamespaceObjectStore")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

fn normalize_prefix(prefix: &str) -> String {
    prefix
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("/")
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

    fn parts_dir(&self) -> PathBuf {
        self.namespace_root().join("parts")
    }

    fn part_dir(&self, part_id: &str) -> PathBuf {
        self.parts_dir().join(part_id)
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
        let mut dir = match fs::read_dir(self.wal_dir()).await {
            Ok(dir) => dir,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => {
                return Err(err).with_context(|| format!("reading WAL dir: {:?}", self.wal_dir()))
            }
        };
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
        let tmp_path = path.with_extension("json.tmp");
        let mut file = fs::File::create(&tmp_path)
            .await
            .with_context(|| format!("creating router file: {:?}", tmp_path))?;
        file.write_all(&encoded)
            .await
            .with_context(|| format!("writing router file: {:?}", tmp_path))?;
        if self.fsync {
            file.sync_all()
                .await
                .with_context(|| format!("fsync router file: {:?}", tmp_path))?;
        }
        drop(file);
        fs::rename(&tmp_path, &path)
            .await
            .with_context(|| format!("renaming router file: {:?} -> {:?}", tmp_path, path))?;
        Ok(())
    }

    /// Persist a part manifest to disk under the namespace parts directory.
    pub async fn write_part_manifest(&self, manifest: &PartManifest) -> Result<PathBuf> {
        let dir = self.parts_dir();
        fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("creating parts directory: {:?}", dir))?;
        let path = dir.join(format!("{}.json", manifest.id));
        let encoded = serde_json::to_vec_pretty(manifest).context("encoding part manifest")?;
        fs::write(&path, encoded)
            .await
            .with_context(|| format!("writing part manifest: {:?}", path))?;
        Ok(path)
    }

    /// Materialize part assets (rows, tombstones, IVF metadata) under the namespace prefix.
    pub async fn write_part_assets(
        &self,
        part_id: &str,
        documents: &[Document],
        deletes: &[String],
    ) -> Result<()> {
        if let Some(store) = &self.object_store {
            return self
                .write_part_assets_object_store(store, part_id, documents, deletes)
                .await;
        }
        self.write_part_assets_local(part_id, documents, deletes)
            .await
    }

    async fn write_part_assets_local(
        &self,
        part_id: &str,
        documents: &[Document],
        deletes: &[String],
    ) -> Result<()> {
        let part_dir = self.part_dir(part_id);
        if let Err(err) = fs::remove_dir_all(&part_dir).await {
            if err.kind() != ErrorKind::NotFound {
                return Err(err)
                    .with_context(|| format!("removing existing part dir: {:?}", part_dir));
            }
        }
        fs::create_dir_all(&part_dir)
            .await
            .with_context(|| format!("creating part directory: {:?}", part_dir))?;

        let segment_dir = part_dir.join("segment");
        let fts_dir = part_dir.join("fts");
        let filters_dir = part_dir.join("filters");
        let ivf_dir = part_dir.join("ivf");
        for dir in [&segment_dir, &fts_dir, &filters_dir, &ivf_dir] {
            fs::create_dir_all(dir)
                .await
                .with_context(|| format!("creating part subdirectory: {:?}", dir))?;
        }

        let rows_bytes = encode_rows_parquet(documents).context("encoding part rows")?;
        let rows_path = segment_dir.join("rows.parquet");
        fs::write(&rows_path, &rows_bytes)
            .await
            .with_context(|| format!("writing rows parquet: {:?}", rows_path))?;

        let tomb_path = segment_dir.join("tombstones.json");
        if deletes.is_empty() {
            if let Err(err) = fs::remove_file(&tomb_path).await {
                if err.kind() != ErrorKind::NotFound {
                    return Err(err)
                        .with_context(|| format!("removing stale tombstones: {:?}", tomb_path));
                }
            }
        } else {
            let tombstones = serde_json::to_vec_pretty(deletes).context("encoding tombstones")?;
            fs::write(&tomb_path, tombstones)
                .await
                .with_context(|| format!("writing tombstones: {:?}", tomb_path))?;
        }

        let postings_dir = ivf_dir.join("postings");
        fs::create_dir_all(&postings_dir)
            .await
            .with_context(|| format!("creating postings dir: {:?}", postings_dir))?;
        let centroids_path = ivf_dir.join("centroids.bin");
        fs::write(&centroids_path, &[])
            .await
            .with_context(|| format!("writing centroids placeholder: {:?}", centroids_path))?;
        let ivf_meta = json!({
            "lists": 0,
            "vectors": documents.len(),
            "tombstones": deletes.len(),
            "generated_at": current_millis(),
        });
        let ivf_meta_path = ivf_dir.join("meta.json");
        fs::write(&ivf_meta_path, serde_json::to_vec_pretty(&ivf_meta)?)
            .await
            .with_context(|| format!("writing ivf meta: {:?}", ivf_meta_path))?;

        let filter_dir = filters_dir.join("bitmaps");
        fs::create_dir_all(&filter_dir)
            .await
            .with_context(|| format!("creating filter dir: {:?}", filter_dir))?;

        Ok(())
    }

    async fn write_part_assets_object_store(
        &self,
        store: &NamespaceObjectStore,
        part_id: &str,
        documents: &[Document],
        deletes: &[String],
    ) -> Result<()> {
        let rows_bytes = encode_rows_parquet(documents).context("encoding part rows")?;
        let rows_path = store.part_path(part_id, &["segment", "rows.parquet"]);
        store
            .put(rows_path, Bytes::from(rows_bytes))
            .await
            .context("uploading part rows")?;

        let tomb_path = store.part_path(part_id, &["segment", "tombstones.json"]);
        if deletes.is_empty() {
            store.delete_if_exists(tomb_path).await?;
        } else {
            let tombstones = serde_json::to_vec_pretty(deletes).context("encoding tombstones")?;
            store
                .put(tomb_path, Bytes::from(tombstones))
                .await
                .context("uploading tombstones")?;
        }

        let centroids_path = store.part_path(part_id, &["ivf", "centroids.bin"]);
        store
            .put(centroids_path, Bytes::new())
            .await
            .context("uploading IVF centroids placeholder")?;
        let ivf_meta = json!({
            "lists": 0,
            "vectors": documents.len(),
            "tombstones": deletes.len(),
            "generated_at": current_millis(),
        });
        let ivf_meta_path = store.part_path(part_id, &["ivf", "meta.json"]);
        store
            .put(
                ivf_meta_path,
                Bytes::from(serde_json::to_vec_pretty(&ivf_meta)?),
            )
            .await
            .context("uploading IVF metadata")?;

        let postings_keep = store.part_path(part_id, &["ivf", "postings", ".keep"]);
        store
            .put(postings_keep, Bytes::new())
            .await
            .context("uploading IVF postings placeholder")?;
        let filters_keep = store.part_path(part_id, &["filters", "bitmaps", ".keep"]);
        store
            .put(filters_keep, Bytes::new())
            .await
            .context("uploading filter placeholder")?;

        Ok(())
    }

    /// Load materialized part assets for compaction/testing.
    pub async fn read_part_assets(&self, part_id: &str) -> Result<(Vec<Document>, Vec<String>)> {
        if let Some(store) = &self.object_store {
            return self.read_part_assets_object_store(store, part_id).await;
        }
        let part_dir = self.part_dir(part_id);
        let segment_dir = part_dir.join("segment");
        let rows_path = segment_dir.join("rows.parquet");
        let docs = match fs::read(&rows_path).await {
            Ok(bytes) => decode_rows_parquet(&bytes)
                .with_context(|| format!("decoding part rows: {:?}", rows_path))?,
            Err(err) if err.kind() == ErrorKind::NotFound => Vec::new(),
            Err(err) => {
                return Err(err).with_context(|| format!("reading part rows: {:?}", rows_path))
            }
        };
        let tomb_path = segment_dir.join("tombstones.json");
        let deletes = match fs::read(&tomb_path).await {
            Ok(bytes) => {
                serde_json::from_slice::<Vec<String>>(&bytes).context("decoding part tombstones")?
            }
            Err(err) if err.kind() == ErrorKind::NotFound => Vec::new(),
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("reading part tombstones: {:?}", tomb_path))
            }
        };
        Ok((docs, deletes))
    }

    async fn read_part_assets_object_store(
        &self,
        store: &NamespaceObjectStore,
        part_id: &str,
    ) -> Result<(Vec<Document>, Vec<String>)> {
        let rows_path = store.part_path(part_id, &["segment", "rows.parquet"]);
        let docs = match store.get(rows_path).await? {
            Some(bytes) => decode_rows_parquet(bytes.as_ref())
                .context("decoding part rows from object store")?,
            None => Vec::new(),
        };
        let tomb_path = store.part_path(part_id, &["segment", "tombstones.json"]);
        let deletes = match store.get(tomb_path).await? {
            Some(bytes) => serde_json::from_slice::<Vec<String>>(bytes.as_ref())
                .context("decoding part tombstones from object store")?,
            None => Vec::new(),
        };
        Ok((docs, deletes))
    }

    async fn remove_part_assets(&self, part_id: &str) -> Result<()> {
        if let Some(store) = &self.object_store {
            let prefix = store.part_prefix(part_id);
            return store.delete_prefix(prefix).await;
        }
        let part_dir = self.part_dir(part_id);
        match fs::remove_dir_all(&part_dir).await {
            Ok(_) => Ok(()),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err).with_context(|| format!("removing part assets: {:?}", part_dir)),
        }
    }

    /// Remove a part manifest if present.
    pub async fn remove_part_manifest(&self, part_id: &str) -> Result<()> {
        let path = self.parts_dir().join(format!("{}.json", part_id));
        match fs::remove_file(&path).await {
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| format!("removing part manifest: {:?}", path))
            }
        }
        self.remove_part_assets(part_id).await
    }

    /// List stored part manifests sorted by WAL start sequence.
    pub async fn list_part_manifests(&self) -> Result<Vec<PartManifest>> {
        let mut manifests = Vec::new();
        let mut dir = match fs::read_dir(self.parts_dir()).await {
            Ok(dir) => dir,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("reading parts directory: {:?}", self.parts_dir()))
            }
        };
        while let Some(entry) = dir.next_entry().await? {
            if !entry.file_type().await?.is_file() {
                continue;
            }
            let bytes = fs::read(entry.path()).await?;
            let manifest: PartManifest = serde_json::from_slice(&bytes)
                .with_context(|| format!("decoding part manifest: {:?}", entry.path()))?;
            manifests.push(manifest);
        }
        manifests.sort_by_key(|manifest| manifest.wal_start);
        Ok(manifests)
    }
}

fn encode_rows_parquet(documents: &[Document]) -> Result<Vec<u8>> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new(
            "vector",
            DataType::List(Arc::new(Field::new("item", DataType::Float32, true))),
            true,
        ),
        Field::new("attributes", DataType::LargeUtf8, true),
    ]));

    let mut id_builder = StringBuilder::new();
    let mut vector_builder = ListBuilder::new(Float32Builder::new());
    let mut attr_builder = LargeStringBuilder::new();
    for doc in documents {
        id_builder.append_value(&doc.id);
        match &doc.vector {
            Some(values) => {
                {
                    let values_builder = vector_builder.values();
                    for value in values {
                        values_builder.append_value(*value);
                    }
                }
                vector_builder.append(true);
            }
            None => {
                vector_builder.append(false);
            }
        }
        match &doc.attributes {
            Some(value) => attr_builder.append_value(value.to_string()),
            None => attr_builder.append_null(),
        }
    }

    let arrays: Vec<ArrayRef> = vec![
        Arc::new(id_builder.finish()),
        Arc::new(vector_builder.finish()),
        Arc::new(attr_builder.finish()),
    ];
    let batch = RecordBatch::try_new(schema.clone(), arrays).context("building record batch")?;

    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::default()))
        .build();
    let mut writer =
        ArrowWriter::try_new(Vec::new(), schema, Some(props)).context("creating parquet writer")?;
    writer.write(&batch).context("writing parquet batch")?;
    let buffer = writer.into_inner().context("extracting parquet buffer")?;
    Ok(buffer)
}

fn decode_rows_parquet(bytes: &[u8]) -> Result<Vec<Document>> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(bytes.to_vec()))
        .context("building parquet reader")?
        .build()
        .context("initializing parquet batch reader")?;

    let mut documents = Vec::new();
    for batch in reader {
        let batch = batch.context("reading parquet batch")?;
        let id_array = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .context("id column must be Utf8")?;
        let vector_array = batch
            .column(1)
            .as_any()
            .downcast_ref::<ListArray>()
            .context("vector column must be List<Float32>")?;
        let attr_array = batch
            .column(2)
            .as_any()
            .downcast_ref::<LargeStringArray>()
            .context("attributes column must be LargeUtf8")?;

        for row in 0..batch.num_rows() {
            let id = id_array.value(row).to_string();
            let vector = if vector_array.is_null(row) {
                None
            } else {
                let values = vector_array.value(row);
                let float_array = values
                    .as_any()
                    .downcast_ref::<Float32Array>()
                    .context("vector values must be Float32")?;
                let mut vec = Vec::with_capacity(float_array.len());
                for idx in 0..float_array.len() {
                    vec.push(float_array.value(idx));
                }
                Some(vec)
            };
            let attributes = if attr_array.is_null(row) {
                None
            } else {
                let raw = attr_array.value(row);
                Some(
                    serde_json::from_str::<serde_json::Value>(raw)
                        .context("decoding document attributes from JSON")?,
                )
            };
            documents.push(Document {
                id,
                vector,
                attributes,
            });
        }
    }

    Ok(documents)
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
    #[serde(default)]
    pub indexed_wal: u64,
    #[serde(default)]
    pub parts: Vec<PartManifest>,
    #[serde(default)]
    pub pin_hot: bool,
}

impl RouterState {
    pub fn new(namespace: String) -> Self {
        Self {
            namespace,
            epoch: 0,
            wal_highwater: 0,
            updated_at: current_millis(),
            indexed_wal: 0,
            parts: Vec::new(),
            pin_hot: false,
        }
    }
}

/// Metadata describing an immutable part produced by the indexer.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PartManifest {
    pub id: String,
    pub wal_start: u64,
    pub wal_end: u64,
    pub rows: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compacted_from: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub tombstones: usize,
}

impl PartManifest {
    pub fn new(id: impl Into<String>, wal_start: u64, wal_end: u64, rows: usize) -> Self {
        Self {
            id: id.into(),
            wal_start,
            wal_end,
            rows,
            compacted_from: Vec::new(),
            tombstones: 0,
        }
    }
}

fn is_zero(value: &usize) -> bool {
    *value == 0
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
    Upsert {
        document: Document,
    },
    Patch {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vector: Option<VectorPatch>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attributes: Option<AttributesPatch>,
    },
    Delete {
        id: String,
    },
    DeleteByFilter {
        filter: FilterExpr,
    },
}

/// Patch operation for vector payloads.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum VectorPatch {
    Set { value: Vec<f32> },
    Remove,
}

/// Patch operation for attribute objects.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AttributesPatch {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub clear: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
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
    use object_store::{
        memory::InMemory, path::Path as ObjectPath, Error as ObjectStoreError, ObjectStore,
    };
    use proptest::prelude::*;

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

    fn temp_store_with_object_store(prefix: &str) -> (PathBuf, NamespaceStore, Arc<InMemory>) {
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
        let memory = Arc::new(InMemory::new());
        let object_store: Arc<dyn ObjectStore> = memory.clone();
        let store = LocalStore::new(&path)
            .with_fsync(false)
            .with_object_store(object_store, prefix.to_string());
        let ns = store.namespace("test");
        (path, ns, memory)
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

    proptest! {
        #[test]
        fn wal_batches_recover_in_sequence(
            payloads in prop::collection::vec(prop::collection::vec(-50i16..50, 1..5), 1..10),
            since in 0u64..15,
        ) {
            let runtime = tokio::runtime::Runtime::new().expect("runtime");
            let result: Result<(), TestCaseError> = runtime.block_on(async move {
                let (dir, ns) = temp_store();
                let mut appended = Vec::new();
                for (idx, payload) in payloads.iter().enumerate() {
                    let seq = (idx + 1) as u64;
                    let operations = payload
                        .iter()
                        .enumerate()
                        .map(|(op_idx, &value)| WriteOp::Upsert {
                            document: Document {
                                id: format!("doc-{seq}-{op_idx}"),
                                vector: Some(vec![value as f32]),
                                attributes: None,
                            },
                        })
                        .collect();
                    let batch = WalBatch {
                        namespace: "test".to_string(),
                        operations,
                    };
                    ns.append_batch(&batch)
                        .await
                        .expect("append batch");
                    appended.push(batch);
                }

                let loaded = ns
                    .load_batches_since(since)
                    .await
                    .expect("load batches");

                let total = appended.len() as u64;
                let expected_sequences: Vec<u64> = if total == 0 || since > total {
                    Vec::new()
                } else if since == 0 {
                    (1..=total).collect()
                } else {
                    (since..=total).collect()
                };

                let actual_sequences: Vec<u64> = loaded.iter().map(|(ptr, _)| ptr.sequence).collect();
                prop_assert_eq!(actual_sequences.as_slice(), expected_sequences.as_slice());

                prop_assert_eq!(loaded.len(), expected_sequences.len());
                let expected_batches: Vec<&WalBatch> = expected_sequences
                    .iter()
                    .map(|seq| &appended[(seq - 1) as usize])
                    .collect();

                for ((pointer, batch), (expected_seq, expected_batch)) in
                    loaded.iter().zip(expected_sequences.iter().zip(expected_batches.iter()))
                {
                    prop_assert_eq!(pointer.namespace.as_str(), "test");
                    prop_assert_eq!(pointer.sequence, *expected_seq);
                    prop_assert_eq!(batch, *expected_batch);
                }

                tokio::fs::remove_dir_all(&dir).await.ok();
                Ok(())
            });
            result?;
        }
    }

    #[tokio::test]
    async fn appending_batches_advances_sequence_and_router() {
        let (_dir, ns) = temp_store();
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

    #[tokio::test]
    async fn load_router_returns_defaults_when_missing() {
        let (dir, ns) = temp_store();
        let router = ns.load_router().await.expect("load router");
        assert_eq!(router.namespace, "test");
        assert_eq!(router.epoch, 0);
        assert_eq!(router.wal_highwater, 0);
        assert!(router.updated_at > 0);
        assert_eq!(router.indexed_wal, 0);
        assert!(router.parts.is_empty());
        assert!(!router.pin_hot);

        tokio::fs::remove_dir_all(dir).await.ok();
    }

    #[tokio::test]
    async fn store_router_persists_state() {
        let (dir, ns) = temp_store();
        let mut router = RouterState::new("test".to_string());
        router.epoch = 4;
        router.wal_highwater = 12;
        router.updated_at = 4242;
        router.indexed_wal = 9;
        router.parts.push(PartManifest::new("p1", 1, 2, 3));
        router.pin_hot = true;
        ns.store_router(&router)
            .await
            .expect("persist router state");

        let loaded = ns.load_router().await.expect("load persisted router");
        assert_eq!(loaded.epoch, 4);
        assert_eq!(loaded.wal_highwater, 12);
        assert_eq!(loaded.updated_at, 4242);
        assert_eq!(loaded.indexed_wal, 9);
        assert_eq!(loaded.parts.len(), 1);
        assert_eq!(loaded.parts[0].id, "p1");
        assert_eq!(loaded.parts[0].rows, 3);
        assert_eq!(loaded.parts[0].tombstones, 0);
        assert!(loaded.pin_hot);

        tokio::fs::remove_dir_all(dir).await.ok();
    }

    #[tokio::test]
    async fn materialize_part_roundtrips_assets() {
        let (dir, ns) = temp_store();
        let documents = vec![
            Document {
                id: "doc-1".to_string(),
                vector: Some(vec![1.0, 2.0, 3.0]),
                attributes: Some(json!({ "foo": "bar" })),
            },
            Document {
                id: "doc-2".to_string(),
                vector: None,
                attributes: None,
            },
        ];
        let deletes = vec!["tomb-1".to_string()];

        ns.write_part_assets("part-check", &documents, &deletes)
            .await
            .expect("write part assets");

        let rows_path = ns.part_dir("part-check").join("segment/rows.parquet");
        assert!(rows_path.exists());

        let (loaded_docs, loaded_deletes) = ns
            .read_part_assets("part-check")
            .await
            .expect("read part assets");
        assert_eq!(loaded_docs, documents);
        assert_eq!(loaded_deletes, deletes);

        ns.remove_part_manifest("part-check")
            .await
            .expect("remove part");
        tokio::fs::remove_dir_all(dir).await.ok();
    }

    #[tokio::test]
    async fn materialize_part_to_object_store() {
        let (dir, ns, memory) = temp_store_with_object_store("cluster/dev");
        let documents = vec![Document {
            id: "doc-remote".to_string(),
            vector: Some(vec![7.0, 8.0, 9.0]),
            attributes: None,
        }];
        let deletes = vec!["gone".to_string()];

        ns.write_part_assets("part-remote", &documents, &deletes)
            .await
            .expect("write remote part");

        let (loaded_docs, loaded_deletes) = ns
            .read_part_assets("part-remote")
            .await
            .expect("read remote part");
        assert_eq!(loaded_docs, documents);
        assert_eq!(loaded_deletes, deletes);

        let rows_path =
            ObjectPath::from("cluster/dev/namespaces/test/parts/part-remote/segment/rows.parquet");
        let bytes = memory
            .get(&rows_path)
            .await
            .expect("rows object")
            .bytes()
            .await
            .expect("rows bytes");
        assert!(!bytes.is_empty());

        ns.remove_part_manifest("part-remote")
            .await
            .expect("remove remote part");
        let removed = memory.get(&rows_path).await;
        assert!(matches!(removed, Err(ObjectStoreError::NotFound { .. })));

        tokio::fs::remove_dir_all(dir).await.ok();
    }

    #[tokio::test]
    async fn part_manifest_round_trip() {
        let (dir, ns) = temp_store();
        let manifest = PartManifest::new("part-1", 1, 3, 10);
        ns.write_part_manifest(&manifest)
            .await
            .expect("write manifest");
        let manifests = ns.list_part_manifests().await.expect("list manifests");
        assert_eq!(manifests, vec![manifest]);
        ns.remove_part_manifest("part-1")
            .await
            .expect("remove manifest");
        let manifests = ns
            .list_part_manifests()
            .await
            .expect("list manifests after remove");
        assert!(manifests.is_empty());
        tokio::fs::remove_dir_all(dir).await.ok();
    }
}
