#![recursion_limit = "4096"]

//! HTTP API surface for elacsym query and admin endpoints.

use std::{
    collections::{BTreeMap, HashSet},
    net::SocketAddr,
    sync::Arc,
};

use anyhow::Result;
use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use elax_core::{
    AnnParams, ConsistencyLevel, DistanceMetric, GroupBy, NamespaceRegistry, Patch, QueryClause,
    QueryRequest, QueryResponse, RankBy, RecallRequest, RecallResponse, WriteBatch, WriteCondition,
    WriteConditionFailed,
};
use elax_filter::FilterExpr;
use elax_store::{AttributesPatch, Document, LocalStore, VectorPatch};
use metrics::counter;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

/// High-level API server wrapper.
#[derive(Clone)]
pub struct ApiServer {
    registry: Arc<NamespaceRegistry>,
}

impl ApiServer {
    /// Construct a new server backed by a [`LocalStore`] root path.
    pub fn new(store: LocalStore) -> Self {
        let registry = NamespaceRegistry::new(store);
        Self {
            registry: Arc::new(registry),
        }
    }

    /// Build the HTTP router for the current server state.
    pub fn router(&self) -> Router<()> {
        let router: Router<()> = Router::new()
            .route("/v2/namespaces/:namespace", post(handle_write))
            .route("/v2/namespaces/:namespace/query", post(handle_query))
            .route(
                "/v1/namespaces/:namespace/_debug/recall",
                post(handle_recall),
            )
            .route("/metrics", get(handle_metrics))
            .with_state(self.registry.clone());
        router
    }

    /// Run the HTTP server until shutdown on the provided address.
    pub async fn run(self, addr: SocketAddr) -> Result<()> {
        let listener = TcpListener::bind(addr).await?;
        axum::serve(listener, self.router().into_make_service()).await?;
        Ok(())
    }

    /// Expose the underlying registry for internal callers/tests.
    pub fn registry(&self) -> Arc<NamespaceRegistry> {
        self.registry.clone()
    }
}

async fn handle_write(
    Path(namespace): Path<String>,
    State(registry): State<Arc<NamespaceRegistry>>,
    Json(payload): Json<WritePayload>,
) -> Result<Json<WriteResponse>, ApiError> {
    const ROUTE: &str = "write";
    let batch = match build_write_batch(namespace.clone(), payload) {
        Ok(batch) => batch,
        Err(err) => {
            let status = err.status.as_u16().to_string();
            counter!(
                "elax_api_requests_total",
                1,
                "route" => ROUTE,
                "status" => status
            );
            return Err(err);
        }
    };

    match registry.apply_write(batch).await {
        Ok(pointer) => {
            let status = StatusCode::OK.as_u16().to_string();
            counter!(
                "elax_api_requests_total",
                1,
                "route" => ROUTE,
                "status" => status
            );
            Ok(Json(WriteResponse {
                wal_sequence: pointer.sequence,
            }))
        }
        Err(err) => match err.downcast::<WriteConditionFailed>() {
            Ok(condition) => {
                let api_err = ApiError::precondition_failed(condition.to_string());
                let status = api_err.status.as_u16().to_string();
                counter!(
                    "elax_api_requests_total",
                    1,
                    "route" => ROUTE,
                    "status" => status
                );
                Err(api_err)
            }
            Err(err) => {
                let api_err: ApiError = err.into();
                let status = api_err.status.as_u16().to_string();
                counter!(
                    "elax_api_requests_total",
                    1,
                    "route" => ROUTE,
                    "status" => status
                );
                Err(api_err)
            }
        },
    }
}

async fn handle_query(
    Path(namespace): Path<String>,
    State(registry): State<Arc<NamespaceRegistry>>,
    Json(payload): Json<QueryPayload>,
) -> Result<Json<QueryResponse>, ApiError> {
    const ROUTE: &str = "query";
    let QueryPayload {
        vector,
        rank_by: initial_rank_by,
        queries,
        top_k,
        metric,
        mut min_wal_sequence,
        ann_params,
        group_by,
        filters,
        filter_bitmap_ids,
        consistency,
    } = payload;

    let mut rank_by = initial_rank_by;
    if rank_by.is_none() {
        if let Some(vector) = vector {
            rank_by = Some(RankBy::vector("vector", vector));
        }
    }

    let consistency = consistency
        .map(|value| value.level)
        .unwrap_or(ConsistencyLevel::Strong);
    if consistency == ConsistencyLevel::Eventual {
        min_wal_sequence = None;
    }

    let request = QueryRequest {
        namespace: namespace.clone(),
        top_k: top_k.unwrap_or(10),
        rank_by,
        queries: queries.unwrap_or_default(),
        metric,
        min_wal_sequence,
        ann_params,
        group_by,
        filters,
        filter_bitmap_ids,
        consistency,
    };

    match registry.query(request).await {
        Ok(response) => {
            let status = StatusCode::OK.as_u16().to_string();
            counter!(
                "elax_api_requests_total",
                1,
                "route" => ROUTE,
                "status" => status
            );
            Ok(Json(response))
        }
        Err(err) => {
            let api_err: ApiError = err.into();
            let status = api_err.status.as_u16().to_string();
            counter!(
                "elax_api_requests_total",
                1,
                "route" => ROUTE,
                "status" => status
            );
            Err(api_err)
        }
    }
}

async fn handle_recall(
    Path(namespace): Path<String>,
    State(registry): State<Arc<NamespaceRegistry>>,
    Json(payload): Json<RecallPayload>,
) -> Result<Json<RecallResponse>, ApiError> {
    const ROUTE: &str = "recall";
    let request = RecallRequest {
        namespace: namespace.clone(),
        num: payload.num,
        top_k: payload.top_k,
        queries: payload.queries,
        ann_params: payload.ann_params,
        metric: payload.metric,
    };

    match registry.debug_recall(request).await {
        Ok(response) => {
            let status = StatusCode::OK.as_u16().to_string();
            counter!(
                "elax_api_requests_total",
                1,
                "route" => ROUTE,
                "status" => status
            );
            Ok(Json(response))
        }
        Err(err) => {
            let api_err: ApiError = err.into();
            let status = api_err.status.as_u16().to_string();
            counter!(
                "elax_api_requests_total",
                1,
                "route" => ROUTE,
                "status" => status
            );
            Err(api_err)
        }
    }
}

async fn handle_metrics() -> Result<impl IntoResponse, ApiError> {
    let body = elax_metrics::gather().map_err(|err| ApiError::internal(err.to_string()))?;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        body,
    ))
}

/// JSON payload for write batches.
#[derive(Debug, Deserialize)]
struct WritePayload {
    #[serde(default, alias = "upserts")]
    upsert_rows: Vec<WriteDocument>,
    #[serde(default)]
    upsert_columns: Vec<ColumnUpsert>,
    #[serde(default)]
    patch_rows: Vec<PatchDocumentPayload>,
    #[serde(default)]
    patch_columns: Vec<ColumnPatchPayload>,
    #[serde(default)]
    deletes: Vec<String>,
    #[serde(default)]
    delete_by_filter: Vec<FilterExpr>,
    #[serde(default)]
    upsert_condition: Option<ConditionPayload>,
    #[serde(default)]
    patch_condition: Option<ConditionPayload>,
    #[serde(default)]
    delete_condition: Option<ConditionPayload>,
}

#[derive(Debug, Deserialize)]
struct ColumnUpsert {
    ids: Vec<String>,
    #[serde(default)]
    vector: Option<Vec<Vec<f32>>>,
    #[serde(default)]
    attributes: BTreeMap<String, Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct ColumnPatchPayload {
    ids: Vec<String>,
    #[serde(default)]
    vector: Option<Vec<Option<Vec<f32>>>>,
    #[serde(default)]
    attributes: BTreeMap<String, Vec<serde_json::Value>>,
    #[serde(default)]
    clear_attributes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PatchDocumentPayload {
    id: String,
    #[serde(default)]
    vector: Option<serde_json::Value>,
    #[serde(default)]
    attributes: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ConditionPayload {
    #[serde(default)]
    min_wal_sequence: Option<u64>,
    #[serde(default)]
    max_wal_sequence: Option<u64>,
}

impl ConditionPayload {
    fn into_condition(self) -> WriteCondition {
        WriteCondition {
            min_wal_sequence: self.min_wal_sequence,
            max_wal_sequence: self.max_wal_sequence,
        }
    }
}

#[derive(Debug, Deserialize)]
struct WriteDocument {
    id: String,
    #[serde(default)]
    vector: Option<Vec<f32>>,
    #[serde(default)]
    attributes: Option<serde_json::Value>,
}

impl WriteDocument {
    fn into_document(self) -> Result<Document, ApiError> {
        if self.id.is_empty() {
            return Err(ApiError::bad_request("id must not be empty"));
        }
        Ok(Document {
            id: self.id,
            vector: self.vector,
            attributes: self.attributes,
        })
    }
}

impl ColumnUpsert {
    fn into_documents(self) -> Result<Vec<Document>, ApiError> {
        if self.ids.is_empty() {
            return Ok(Vec::new());
        }
        let len = self.ids.len();
        if let Some(vectors) = &self.vector {
            if vectors.len() != len {
                return Err(ApiError::bad_request(
                    "upsert_columns.vector length must match ids",
                ));
            }
        }
        for (field, values) in &self.attributes {
            if values.len() != len {
                return Err(ApiError::bad_request(format!(
                    "upsert_columns.{field} length must match ids"
                )));
            }
        }

        let mut docs = Vec::with_capacity(len);
        for (index, id) in self.ids.into_iter().enumerate() {
            if id.is_empty() {
                return Err(ApiError::bad_request("id must not be empty"));
            }
            let vector = self.vector.as_ref().map(|vectors| vectors[index].clone());
            let mut attributes = serde_json::Map::new();
            for (field, values) in &self.attributes {
                attributes.insert(field.clone(), values[index].clone());
            }
            docs.push(Document {
                id,
                vector,
                attributes: if attributes.is_empty() {
                    None
                } else {
                    Some(serde_json::Value::Object(attributes))
                },
            });
        }
        Ok(docs)
    }
}

impl ColumnPatchPayload {
    fn into_patches(self) -> Result<Vec<Patch>, ApiError> {
        if self.ids.is_empty() {
            return Ok(Vec::new());
        }
        let len = self.ids.len();
        if let Some(vectors) = &self.vector {
            if vectors.len() != len {
                return Err(ApiError::bad_request(
                    "patch_columns.vector length must match ids",
                ));
            }
        }
        for (field, values) in &self.attributes {
            if values.len() != len {
                return Err(ApiError::bad_request(format!(
                    "patch_columns.{field} length must match ids"
                )));
            }
        }

        let clear: HashSet<String> = self.clear_attributes.into_iter().collect();
        let mut patches = Vec::with_capacity(len);
        for (index, id) in self.ids.into_iter().enumerate() {
            if id.is_empty() {
                return Err(ApiError::bad_request("id must not be empty"));
            }
            let vector = self.vector.as_ref().map(|vectors| match &vectors[index] {
                Some(values) => VectorPatch::Set {
                    value: values.clone(),
                },
                None => VectorPatch::Remove,
            });

            let mut attributes_patch = AttributesPatch {
                set: BTreeMap::new(),
                remove: Vec::new(),
                clear: clear.contains(&id),
            };
            for (field, values) in &self.attributes {
                let value = values[index].clone();
                if value.is_null() {
                    attributes_patch.remove.push(field.clone());
                } else {
                    attributes_patch.set.insert(field.clone(), value);
                }
            }
            let has_attr = attributes_patch.clear
                || !attributes_patch.set.is_empty()
                || !attributes_patch.remove.is_empty();

            patches.push(Patch {
                id,
                vector,
                attributes: if has_attr {
                    Some(attributes_patch)
                } else {
                    None
                },
            });
        }
        Ok(patches)
    }
}

impl PatchDocumentPayload {
    fn into_patch(self) -> Result<Patch, ApiError> {
        if self.id.is_empty() {
            return Err(ApiError::bad_request("id must not be empty"));
        }
        let vector = match self.vector {
            None => None,
            Some(value) => Some(parse_vector_patch_value(value)?),
        };

        let attributes = match self.attributes {
            None => None,
            Some(serde_json::Value::Null) => Some(AttributesPatch {
                set: BTreeMap::new(),
                remove: Vec::new(),
                clear: true,
            }),
            Some(serde_json::Value::Object(map)) => {
                let mut set = BTreeMap::new();
                let mut remove = Vec::new();
                for (key, value) in map {
                    if value.is_null() {
                        remove.push(key);
                    } else {
                        set.insert(key, value);
                    }
                }
                if set.is_empty() && remove.is_empty() {
                    None
                } else {
                    Some(AttributesPatch {
                        set,
                        remove,
                        clear: false,
                    })
                }
            }
            Some(_) => {
                return Err(ApiError::bad_request(
                    "patch_rows.attributes must be an object or null",
                ))
            }
        };

        Ok(Patch {
            id: self.id,
            vector,
            attributes,
        })
    }
}

fn parse_vector_patch_value(value: serde_json::Value) -> Result<VectorPatch, ApiError> {
    if value.is_null() {
        return Ok(VectorPatch::Remove);
    }
    let array = match value {
        serde_json::Value::Array(items) => items,
        _ => {
            return Err(ApiError::bad_request(
                "vector patch must be null or an array of numbers",
            ))
        }
    };
    let mut values = Vec::with_capacity(array.len());
    for item in array {
        let number = item
            .as_f64()
            .ok_or_else(|| ApiError::bad_request("vector patch entries must be numeric values"))?;
        values.push(number as f32);
    }
    Ok(VectorPatch::Set { value: values })
}

fn build_write_batch(namespace: String, payload: WritePayload) -> Result<WriteBatch, ApiError> {
    let WritePayload {
        upsert_rows,
        upsert_columns,
        patch_rows,
        patch_columns,
        deletes,
        delete_by_filter,
        upsert_condition,
        patch_condition,
        delete_condition,
    } = payload;

    let mut upserts = Vec::new();
    for doc in upsert_rows {
        upserts.push(doc.into_document()?);
    }
    for column in upsert_columns {
        upserts.extend(column.into_documents()?);
    }

    let mut patches = Vec::new();
    for patch in patch_rows {
        patches.push(patch.into_patch()?);
    }
    for column in patch_columns {
        patches.extend(column.into_patches()?);
    }

    Ok(WriteBatch {
        namespace,
        upserts,
        patches,
        deletes,
        delete_filters: delete_by_filter,
        upsert_condition: upsert_condition.map(ConditionPayload::into_condition),
        patch_condition: patch_condition.map(ConditionPayload::into_condition),
        delete_condition: delete_condition.map(ConditionPayload::into_condition),
    })
}

#[derive(Debug, Deserialize)]
struct QueryPayload {
    #[serde(default)]
    vector: Option<Vec<f32>>,
    #[serde(default)]
    rank_by: Option<RankBy>,
    #[serde(default)]
    queries: Option<Vec<QueryClause>>,
    #[serde(default)]
    top_k: Option<usize>,
    #[serde(default)]
    metric: Option<DistanceMetric>,
    #[serde(default)]
    min_wal_sequence: Option<u64>,
    #[serde(default)]
    ann_params: AnnParams,
    #[serde(default)]
    group_by: Option<GroupBy>,
    #[serde(default)]
    filters: Option<FilterExpr>,
    #[serde(default)]
    filter_bitmap_ids: Option<Vec<String>>,
    #[serde(default)]
    consistency: Option<ConsistencyPayload>,
}

#[derive(Debug, Deserialize)]
struct ConsistencyPayload {
    #[serde(default = "default_consistency_level")]
    level: ConsistencyLevel,
}

fn default_consistency_level() -> ConsistencyLevel {
    ConsistencyLevel::Strong
}

#[derive(Debug, Deserialize)]
struct RecallPayload {
    #[serde(default = "default_recall_num")]
    num: usize,
    #[serde(default = "default_recall_top_k")]
    top_k: usize,
    #[serde(default)]
    queries: Option<Vec<Vec<f32>>>,
    #[serde(default)]
    metric: Option<DistanceMetric>,
    #[serde(default)]
    ann_params: AnnParams,
}

fn default_recall_num() -> usize {
    10
}

fn default_recall_top_k() -> usize {
    10
}

#[derive(Debug, Serialize)]
struct WriteResponse {
    wal_sequence: u64,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }

    fn precondition_failed(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PRECONDITION_FAILED,
            message: msg.into(),
        }
    }

    fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
        }
    }
}

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(err: E) -> Self {
        let err = err.into();
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: err.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let body = Json(serde_json::json!({
            "error": self.message,
        }));
        (self.status, body).into_response()
    }
}
