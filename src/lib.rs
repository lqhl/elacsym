//! Elacsym - An open-source vector database built on object storage
//!
//! Elacsym is inspired by turbopuffer and designed to provide:
//! - Scalable vector search using RaBitQ
//! - Cost-effective storage using S3/object storage
//! - Hybrid caching with foyer (memory + disk)
//! - Full-text search with tantivy
//! - Simple HTTP API

pub mod api;
pub mod cache;
pub mod config;
pub mod error;
pub mod index;
pub mod manifest;
pub mod namespace;
pub mod query;
pub mod segment;
pub mod sharding;
pub mod storage;
pub mod types;
pub mod wal;

pub use error::{Error, Result};
