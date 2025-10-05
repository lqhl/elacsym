//! Query execution engine

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::types::{AttributeValue, DistanceMetric, DocId, Vector};

/// Query request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector: Option<Vector>,
    #[serde(default = "default_top_k")]
    pub top_k: usize,
    #[serde(default = "default_metric")]
    pub metric: DistanceMetric,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_text: Option<FullTextQuery>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<FilterExpression>,
    #[serde(default)]
    pub include_vector: bool,
    #[serde(default)]
    pub include_attributes: Vec<String>,
}

fn default_top_k() -> usize {
    10
}

fn default_metric() -> DistanceMetric {
    DistanceMetric::Cosine
}

/// Full-text search query
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FullTextQuery {
    pub field: String,
    pub query: String,
    #[serde(default = "default_weight")]
    pub weight: f32,
}

fn default_weight() -> f32 {
    0.5
}

/// Filter expression
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum FilterExpression {
    And { conditions: Vec<FilterCondition> },
    Or { conditions: Vec<FilterCondition> },
}

/// Filter condition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterCondition {
    pub field: String,
    pub op: FilterOp,
    pub value: AttributeValue,
}

/// Filter operation
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilterOp {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
    Contains,
    ContainsAny,
}

/// Query response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResponse {
    pub results: Vec<QueryResult>,
    pub took_ms: u64,
}

/// Single query result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub id: DocId,
    pub score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector: Option<Vector>,
    #[serde(default)]
    pub attributes: HashMap<String, AttributeValue>,
}
