//! API request/response models

use elacsym_core::Metadata;
use serde::{Deserialize, Serialize};

/// Upsert request
#[derive(Debug, Serialize, Deserialize)]
pub struct UpsertRequest {
    pub vectors: Vec<VectorData>,
    #[serde(default)]
    pub namespace: String,
}

/// Vector data for upsert
#[derive(Debug, Serialize, Deserialize)]
pub struct VectorData {
    pub id: Option<String>,
    pub values: Vec<f32>,
    #[serde(default)]
    pub metadata: Metadata,
}

/// Query request
#[derive(Debug, Serialize, Deserialize)]
pub struct QueryRequest {
    pub vector: Vec<f32>,
    #[serde(default = "default_top_k")]
    pub top_k: usize,
    #[serde(default)]
    pub namespace: String,
    #[serde(default)]
    pub include_values: bool,
    #[serde(default = "default_true")]
    pub include_metadata: bool,
}

fn default_top_k() -> usize {
    10
}

fn default_true() -> bool {
    true
}

/// Query response
#[derive(Debug, Serialize, Deserialize)]
pub struct QueryResponse {
    pub matches: Vec<ScoredVectorData>,
}

/// Scored vector data
#[derive(Debug, Serialize, Deserialize)]
pub struct ScoredVectorData {
    pub id: String,
    pub score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<f32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
}

/// Fetch request
#[derive(Debug, Serialize, Deserialize)]
pub struct FetchRequest {
    pub ids: Vec<String>,
    #[serde(default)]
    pub namespace: String,
}

/// Delete request
#[derive(Debug, Serialize, Deserialize)]
pub struct DeleteRequest {
    pub ids: Vec<String>,
    #[serde(default)]
    pub namespace: String,
}

/// Create namespace request
#[derive(Debug, Serialize, Deserialize)]
pub struct CreateNamespaceRequest {
    pub name: String,
    pub dimension: usize,
}

/// Error response
#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
    pub message: String,
}
