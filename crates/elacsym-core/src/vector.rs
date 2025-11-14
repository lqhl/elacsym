//! Vector data structures

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Unique identifier for a vector
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VectorId(String);

impl VectorId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn generate() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for VectorId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for VectorId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl std::fmt::Display for VectorId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Metadata value types
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MetadataValue {
    String(String),
    Number(f64),
    Boolean(bool),
    Null,
}

impl From<String> for MetadataValue {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<&str> for MetadataValue {
    fn from(s: &str) -> Self {
        Self::String(s.to_string())
    }
}

impl From<f64> for MetadataValue {
    fn from(n: f64) -> Self {
        Self::Number(n)
    }
}

impl From<bool> for MetadataValue {
    fn from(b: bool) -> Self {
        Self::Boolean(b)
    }
}

/// Vector metadata (key-value pairs)
pub type Metadata = HashMap<String, MetadataValue>;

/// A vector with its metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vector {
    /// Unique identifier
    pub id: VectorId,

    /// Vector values (embeddings)
    pub values: Vec<f32>,

    /// Metadata associated with the vector
    #[serde(default)]
    pub metadata: Metadata,

    /// Namespace this vector belongs to
    pub namespace: String,
}

impl Vector {
    pub fn new(
        id: VectorId,
        values: Vec<f32>,
        metadata: Metadata,
        namespace: impl Into<String>,
    ) -> Self {
        Self {
            id,
            values,
            metadata,
            namespace: namespace.into(),
        }
    }

    pub fn dimension(&self) -> usize {
        self.values.len()
    }
}

/// Query result with score
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredVector {
    pub id: VectorId,
    pub score: f32,
    pub vector: Option<Vector>,
}

impl ScoredVector {
    pub fn new(id: VectorId, score: f32, vector: Option<Vector>) -> Self {
        Self { id, score, vector }
    }
}

/// Batch of vectors for upsert
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorBatch {
    pub vectors: Vec<Vector>,
}

impl VectorBatch {
    pub fn new(vectors: Vec<Vector>) -> Self {
        Self { vectors }
    }

    pub fn len(&self) -> usize {
        self.vectors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vectors.is_empty()
    }
}
