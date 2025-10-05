//! Core types for elacsym

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Document ID type
pub type DocId = u64;

/// Vector type
pub type Vector = Vec<f32>;

/// Attribute value types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum AttributeValue {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    StringArray(Vec<String>),
}

/// Document represents a single record in the database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: DocId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector: Option<Vector>,
    #[serde(default)]
    pub attributes: HashMap<String, AttributeValue>,
}

/// Distance metric for vector search
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DistanceMetric {
    Cosine,
    L2,
    Dot,
}

/// Attribute type in schema
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AttributeType {
    String,
    Integer,
    Float,
    Boolean,
    #[serde(rename = "array<string>")]
    StringArray,
}

/// Attribute schema configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributeSchema {
    #[serde(rename = "type")]
    pub attr_type: AttributeType,
    #[serde(default)]
    pub indexed: bool,
    #[serde(default)]
    pub full_text: bool,
}

/// Namespace schema
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schema {
    pub vector_dim: usize,
    pub vector_metric: DistanceMetric,
    pub attributes: HashMap<String, AttributeSchema>,
}

/// Segment metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentInfo {
    pub segment_id: String,
    pub file_path: String,
    pub row_count: usize,
    pub id_range: (DocId, DocId),
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Tombstone: marks deleted documents
    #[serde(default)]
    pub tombstones: Vec<DocId>,
}

/// Namespace statistics
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NamespaceStats {
    pub total_docs: usize,
    pub total_size_bytes: u64,
    pub segment_count: usize,
}
