#![allow(dead_code)]

//! Text search integration glue (tantivy and filters).

use common::Result;

/// Placeholder trait representing a text index capable of contributing snippets.
pub trait TextIndex: Send + Sync {
    fn search(&self, _query: &str) -> Result<Vec<String>>;
}
