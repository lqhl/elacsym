//! Error types for elacsym

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Vector dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },

    #[error("Vector not found: {0}")]
    VectorNotFound(String),

    #[error("Namespace not found: {0}")]
    NamespaceNotFound(String),

    #[error("Slab not found: {0}")]
    SlabNotFound(String),

    #[error("Invalid vector id: {0}")]
    InvalidVectorId(String),

    #[error("Invalid namespace: {0}")]
    InvalidNamespace(String),

    #[error("Storage error: {0}")]
    Storage(#[from] anyhow::Error),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Index error: {0}")]
    Index(String),

    #[error("Metadata error: {0}")]
    Metadata(String),

    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
