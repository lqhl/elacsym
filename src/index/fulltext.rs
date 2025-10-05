//! Full-text search index using Tantivy

use crate::types::DocId;
use crate::Result;

/// Full-text search index
pub struct FullTextIndex {
    field_name: String,
}

impl FullTextIndex {
    pub fn new(field_name: String) -> Result<Self> {
        Ok(Self { field_name })
    }

    /// Add documents to the index
    pub fn add(&mut self, doc_id: DocId, text: &str) -> Result<()> {
        // TODO: Integrate with Tantivy
        todo!("Implement Tantivy integration")
    }

    /// Search for documents matching query
    pub fn search(&self, query: &str, top_k: usize) -> Result<Vec<(DocId, f32)>> {
        // TODO: Implement Tantivy search
        todo!("Implement Tantivy search")
    }
}
