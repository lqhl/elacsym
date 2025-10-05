//! Full-text search index using Tantivy
//!
//! Provides BM25-based full-text search for string attributes.

use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::*;
use tantivy::{doc, Index, IndexReader, IndexWriter, ReloadPolicy};
use std::path::Path;

use crate::types::DocId;
use crate::{Error, Result};

/// Full-text search index for a single field
pub struct FullTextIndex {
    index: Index,
    reader: IndexReader,
    writer: IndexWriter,
    id_field: Field,
    text_field: Field,
    field_name: String,
}

impl FullTextIndex {
    /// Create a new in-memory full-text index
    pub fn new(field_name: String) -> Result<Self> {
        let mut schema_builder = Schema::builder();

        // ID field - used to map back to document IDs
        let id_field = schema_builder.add_u64_field("id", INDEXED | STORED);

        // Text field - the actual searchable content
        let text_field = schema_builder.add_text_field(&field_name, TEXT | STORED);

        let schema = schema_builder.build();

        // Create in-memory index
        let index = Index::create_in_ram(schema);

        // Create reader with auto-reload
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .map_err(|e| Error::internal(format!("Failed to create index reader: {}", e)))?;

        // Create writer with 50MB buffer
        let writer = index
            .writer(50_000_000)
            .map_err(|e| Error::internal(format!("Failed to create index writer: {}", e)))?;

        Ok(Self {
            index,
            reader,
            writer,
            id_field,
            text_field,
            field_name,
        })
    }

    /// Create a persistent full-text index on disk
    pub fn new_persistent<P: AsRef<Path>>(field_name: String, path: P) -> Result<Self> {
        let mut schema_builder = Schema::builder();

        let id_field = schema_builder.add_u64_field("id", INDEXED | STORED);
        let text_field = schema_builder.add_text_field(&field_name, TEXT | STORED);

        let schema = schema_builder.build();

        // Create directory if it doesn't exist
        std::fs::create_dir_all(&path)
            .map_err(|e| Error::internal(format!("Failed to create index directory: {}", e)))?;

        // Create or open index
        let index = Index::create_in_dir(&path, schema.clone())
            .or_else(|_| Index::open_in_dir(&path))
            .map_err(|e| Error::internal(format!("Failed to create/open index: {}", e)))?;

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .map_err(|e| Error::internal(format!("Failed to create index reader: {}", e)))?;

        let writer = index
            .writer(50_000_000)
            .map_err(|e| Error::internal(format!("Failed to create index writer: {}", e)))?;

        Ok(Self {
            index,
            reader,
            writer,
            id_field,
            text_field,
            field_name,
        })
    }

    /// Add documents to the index
    pub fn add_documents(&mut self, docs: &[(DocId, String)]) -> Result<()> {
        for (id, text) in docs {
            let document = doc!(
                self.id_field => *id,
                self.text_field => text.clone()
            );

            self.writer
                .add_document(document)
                .map_err(|e| Error::internal(format!("Failed to add document: {}", e)))?;
        }

        // Commit the changes
        self.writer
            .commit()
            .map_err(|e| Error::internal(format!("Failed to commit index: {}", e)))?;

        // Reload reader to see committed changes
        self.reader
            .reload()
            .map_err(|e| Error::internal(format!("Failed to reload index reader: {}", e)))?;

        Ok(())
    }

    /// Add a single document (convenience method)
    pub fn add(&mut self, doc_id: DocId, text: &str) -> Result<()> {
        self.add_documents(&[(doc_id, text.to_string())])
    }

    /// Search using BM25 algorithm
    ///
    /// Returns list of (doc_id, bm25_score) sorted by score descending
    pub fn search(&self, query_text: &str, top_k: usize) -> Result<Vec<(DocId, f32)>> {
        let searcher = self.reader.searcher();

        // Parse query
        let query_parser = QueryParser::for_index(&self.index, vec![self.text_field]);
        let query = query_parser
            .parse_query(query_text)
            .map_err(|e| Error::internal(format!("Failed to parse query: {}", e)))?;

        // Execute search
        let top_docs = searcher
            .search(&query, &TopDocs::with_limit(top_k))
            .map_err(|e| Error::internal(format!("Failed to execute search: {}", e)))?;

        // Extract results
        let mut results = Vec::new();
        for (score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher
                .doc(doc_address)
                .map_err(|e| Error::internal(format!("Failed to retrieve document: {}", e)))?;

            // Extract document ID
            let id = doc
                .get_first(self.id_field)
                .and_then(|v| v.as_u64())
                .ok_or_else(|| Error::internal("Document missing ID field"))?;

            results.push((id, score));
        }

        Ok(results)
    }

    /// Get the field name this index is for
    pub fn field_name(&self) -> &str {
        &self.field_name
    }

    /// Get the number of documents in the index
    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fulltext_index_basic() {
        let mut index = FullTextIndex::new("content".to_string()).unwrap();

        // Add documents
        let docs = vec![
            (1, "The quick brown fox jumps over the lazy dog".to_string()),
            (2, "A fast brown fox leaps over a sleeping dog".to_string()),
            (3, "The lazy cat sleeps all day".to_string()),
        ];

        index.add_documents(&docs).unwrap();

        // Search for "fox"
        let results = index.search("fox", 10).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().any(|(id, _)| *id == 1));
        assert!(results.iter().any(|(id, _)| *id == 2));

        // Search for "cat"
        let results = index.search("cat", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 3);
    }

    #[test]
    fn test_fulltext_index_bm25_scoring() {
        let mut index = FullTextIndex::new("title".to_string()).unwrap();

        let docs = vec![
            (1, "Rust programming language".to_string()),
            (2, "Rust vector database".to_string()),
            (3, "Python programming language".to_string()),
        ];

        index.add_documents(&docs).unwrap();

        // Search for "rust" - should return docs 1 and 2
        let results = index.search("rust", 10).unwrap();
        assert_eq!(results.len(), 2);

        // Verify both contain "rust"
        let ids: Vec<u64> = results.iter().map(|(id, _)| *id).collect();
        assert!(ids.contains(&1));
        assert!(ids.contains(&2));
    }

    #[test]
    fn test_fulltext_index_phrase_query() {
        let mut index = FullTextIndex::new("content".to_string()).unwrap();

        let docs = vec![
            (1, "vector database for machine learning".to_string()),
            (2, "database vector search".to_string()),
            (3, "machine learning models".to_string()),
        ];

        index.add_documents(&docs).unwrap();

        // Search for phrase
        let results = index.search("\"vector database\"", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 1);
    }

    #[test]
    fn test_fulltext_index_multiple_terms() {
        let mut index = FullTextIndex::new("content".to_string()).unwrap();

        let docs = vec![
            (1, "rust programming".to_string()),
            (2, "rust database".to_string()),
            (3, "go programming".to_string()),
        ];

        index.add_documents(&docs).unwrap();

        // Search for "rust programming"
        let results = index.search("rust programming", 10).unwrap();

        // Should return all docs with "rust" or "programming"
        assert!(results.len() >= 1);

        // Doc 1 should have highest score (contains both terms)
        assert_eq!(results[0].0, 1);
    }

    #[test]
    fn test_fulltext_index_num_docs() {
        let mut index = FullTextIndex::new("text".to_string()).unwrap();

        assert_eq!(index.num_docs(), 0);

        let docs = vec![
            (1, "document one".to_string()),
            (2, "document two".to_string()),
        ];

        index.add_documents(&docs).unwrap();
        assert_eq!(index.num_docs(), 2);
    }
}
