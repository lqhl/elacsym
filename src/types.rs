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

/// Full-text search configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FullTextConfig {
    /// Simple boolean flag (backward compatible)
    Simple(bool),
    /// Advanced configuration
    Advanced {
        #[serde(default = "default_language")]
        language: String,
        #[serde(default = "default_true")]
        stemming: bool,
        #[serde(default = "default_true")]
        remove_stopwords: bool,
        #[serde(default)]
        case_sensitive: bool,
        #[serde(default = "default_tokenizer")]
        tokenizer: String,
    },
}

fn default_language() -> String {
    "english".to_string()
}

fn default_true() -> bool {
    true
}

fn default_tokenizer() -> String {
    "default".to_string()
}

impl Default for FullTextConfig {
    fn default() -> Self {
        FullTextConfig::Simple(false)
    }
}

impl FullTextConfig {
    /// Check if full-text search is enabled
    pub fn is_enabled(&self) -> bool {
        match self {
            FullTextConfig::Simple(enabled) => *enabled,
            FullTextConfig::Advanced { .. } => true,
        }
    }

    /// Get language setting
    pub fn language(&self) -> &str {
        match self {
            FullTextConfig::Simple(_) => "english",
            FullTextConfig::Advanced { language, .. } => language,
        }
    }

    /// Get stemming setting
    pub fn stemming(&self) -> bool {
        match self {
            FullTextConfig::Simple(_) => true,
            FullTextConfig::Advanced { stemming, .. } => *stemming,
        }
    }

    /// Get stopwords setting
    pub fn remove_stopwords(&self) -> bool {
        match self {
            FullTextConfig::Simple(_) => true,
            FullTextConfig::Advanced {
                remove_stopwords, ..
            } => *remove_stopwords,
        }
    }

    /// Get case sensitivity setting
    pub fn case_sensitive(&self) -> bool {
        match self {
            FullTextConfig::Simple(_) => false,
            FullTextConfig::Advanced { case_sensitive, .. } => *case_sensitive,
        }
    }
}

/// Attribute schema configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributeSchema {
    #[serde(rename = "type")]
    pub attr_type: AttributeType,
    #[serde(default)]
    pub indexed: bool,
    #[serde(default)]
    pub full_text: FullTextConfig,
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
    /// Vector index path (RaBitQ)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_index_path: Option<String>,
    /// Full-text index paths (field_name -> index_path)
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub fulltext_index_paths: HashMap<String, String>,
}

/// Namespace statistics
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NamespaceStats {
    pub total_docs: usize,
    pub total_size_bytes: u64,
    pub segment_count: usize,
}
