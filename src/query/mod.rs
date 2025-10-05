//! Query execution engine

pub mod executor;
pub mod fusion;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::types::{AttributeValue, DistanceMetric, DocId, Vector};

pub use executor::FilterExecutor;

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
#[serde(untagged)]
pub enum FullTextQuery {
    /// Single field search
    Single {
        field: String,
        query: String,
        #[serde(default = "default_weight")]
        weight: f32,
    },
    /// Multi-field search with per-field weights
    Multi {
        fields: Vec<String>,
        query: String,
        #[serde(default)]
        weights: std::collections::HashMap<String, f32>,
    },
}

impl FullTextQuery {
    /// Get the query text
    pub fn query_text(&self) -> &str {
        match self {
            FullTextQuery::Single { query, .. } => query,
            FullTextQuery::Multi { query, .. } => query,
        }
    }

    /// Get all fields involved in the query
    pub fn fields(&self) -> Vec<&str> {
        match self {
            FullTextQuery::Single { field, .. } => vec![field.as_str()],
            FullTextQuery::Multi { fields, .. } => fields.iter().map(|s| s.as_str()).collect(),
        }
    }

    /// Get weight for a specific field
    pub fn field_weight(&self, field_name: &str) -> f32 {
        match self {
            FullTextQuery::Single { field, weight, .. } => {
                if field == field_name {
                    *weight
                } else {
                    0.0
                }
            }
            FullTextQuery::Multi { weights, .. } => {
                weights.get(field_name).copied().unwrap_or(1.0)
            }
        }
    }
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
