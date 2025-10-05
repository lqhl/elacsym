//! Error types for elacsym

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Index error: {0}")]
    Index(String),

    #[error("Namespace not found: {0}")]
    NamespaceNotFound(String),

    #[error("Invalid schema: {0}")]
    InvalidSchema(String),

    #[error("Invalid query: {0}")]
    InvalidQuery(String),

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    #[error("Parquet error: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl Error {
    pub fn storage(msg: impl Into<String>) -> Self {
        Error::Storage(msg.into())
    }

    pub fn index(msg: impl Into<String>) -> Self {
        Error::Index(msg.into())
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Error::Internal(msg.into())
    }
}
