//! HTTP API surface for elacsym query and admin endpoints.

use std::{net::SocketAddr, sync::Arc};

use anyhow::Result;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use elax_core::{
    AnnParams, DistanceMetric, NamespaceRegistry, QueryRequest, QueryResponse, WriteBatch,
};
use elax_store::{Document, LocalStore};
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
    let upserts = payload
        .upserts
        .into_iter()
        .map(WriteDocument::into_document)
        .collect::<Result<Vec<_>, _>>()?;
    let batch = WriteBatch {
        namespace: namespace.clone(),
        upserts,
        deletes: payload.deletes.unwrap_or_default(),
    };
    let pointer = registry.apply_write(batch).await?;
    Ok(Json(WriteResponse {
        wal_sequence: pointer.sequence,
    }))
}

async fn handle_query(
    Path(namespace): Path<String>,
    State(registry): State<Arc<NamespaceRegistry>>,
    Json(payload): Json<QueryPayload>,
) -> Result<Json<QueryResponse>, ApiError> {
    let request = QueryRequest {
        namespace: namespace.clone(),
        vector: payload.vector,
        top_k: payload.top_k.unwrap_or(10),
        metric: payload.metric,
        min_wal_sequence: payload.min_wal_sequence,
        ann_params: payload.ann_params,
    };
    let response = registry.query(request).await?;
    Ok(Json(response))
}

/// JSON payload for write batches.
#[derive(Debug, Deserialize)]
struct WritePayload {
    #[serde(default)]
    upserts: Vec<WriteDocument>,
    deletes: Option<Vec<String>>,
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

#[derive(Debug, Deserialize)]
struct QueryPayload {
    vector: Vec<f32>,
    #[serde(default)]
    top_k: Option<usize>,
    #[serde(default)]
    metric: Option<DistanceMetric>,
    #[serde(default)]
    min_wal_sequence: Option<u64>,
    #[serde(default)]
    ann_params: AnnParams,
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
