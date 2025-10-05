//! Full-text search index using Tantivy
//!
//! Provides BM25-based full-text search for string attributes.

use std::path::Path;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::*;
use tantivy::tokenizer::{
    LowerCaser, RemoveLongFilter, SimpleTokenizer, Stemmer, StopWordFilter, TextAnalyzer,
};
use tantivy::{doc, Index, IndexReader, IndexWriter, ReloadPolicy};

use crate::types::{DocId, FullTextConfig};
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
        // Use default config (Simple(true) with english, stemming, stopwords)
        let config = FullTextConfig::Advanced {
            language: "english".to_string(),
            stemming: true,
            remove_stopwords: true,
            case_sensitive: false,
            tokenizer: "default".to_string(),
        };
        Self::new_with_config(field_name, config)
    }

    /// Create a new in-memory full-text index with custom configuration
    pub fn new_with_config(field_name: String, config: FullTextConfig) -> Result<Self> {
        // Create in-memory index first (need it for tokenizer registration)
        let index = Index::create_in_ram(Schema::builder().build());

        // Register custom analyzer based on config
        let analyzer_name = Self::register_analyzer(&index, &config)?;

        // Now build schema with the custom analyzer
        let mut schema_builder = Schema::builder();

        // ID field - used to map back to document IDs
        let id_field = schema_builder.add_u64_field("id", INDEXED | STORED);

        // Text field - with custom analyzer
        let text_options = TextOptions::default()
            .set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer(&analyzer_name)
                    .set_index_option(IndexRecordOption::WithFreqsAndPositions),
            )
            .set_stored();
        let text_field = schema_builder.add_text_field(&field_name, text_options);

        let schema = schema_builder.build();

        // Recreate index with correct schema
        let index = Index::create_in_ram(schema);

        // Re-register analyzer on new index
        Self::register_analyzer(&index, &config)?;

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

    /// Register custom text analyzer based on FullTextConfig
    /// Returns the name of the registered analyzer
    fn register_analyzer(index: &Index, config: &FullTextConfig) -> Result<String> {
        let analyzer_name = "custom".to_string();

        // Map language string to Tantivy Language enum
        let language = match config.language() {
            "arabic" => tantivy::tokenizer::Language::Arabic,
            "danish" => tantivy::tokenizer::Language::Danish,
            "dutch" => tantivy::tokenizer::Language::Dutch,
            "english" => tantivy::tokenizer::Language::English,
            "finnish" => tantivy::tokenizer::Language::Finnish,
            "french" => tantivy::tokenizer::Language::French,
            "german" => tantivy::tokenizer::Language::German,
            "greek" => tantivy::tokenizer::Language::Greek,
            "hungarian" => tantivy::tokenizer::Language::Hungarian,
            "italian" => tantivy::tokenizer::Language::Italian,
            "norwegian" => tantivy::tokenizer::Language::Norwegian,
            "portuguese" => tantivy::tokenizer::Language::Portuguese,
            "romanian" => tantivy::tokenizer::Language::Romanian,
            "russian" => tantivy::tokenizer::Language::Russian,
            "spanish" => tantivy::tokenizer::Language::Spanish,
            "swedish" => tantivy::tokenizer::Language::Swedish,
            "tamil" => tantivy::tokenizer::Language::Tamil,
            "turkish" => tantivy::tokenizer::Language::Turkish,
            _ => tantivy::tokenizer::Language::English, // Default to English
        };

        // Build analyzer chain - must be built in one go due to type changes
        let analyzer = if config.case_sensitive() {
            // Case-sensitive path
            if config.remove_stopwords() && config.stemming() {
                TextAnalyzer::builder(SimpleTokenizer::default())
                    .filter(StopWordFilter::new(language).unwrap())
                    .filter(Stemmer::new(language))
                    .filter(RemoveLongFilter::limit(40))
                    .build()
            } else if config.remove_stopwords() {
                TextAnalyzer::builder(SimpleTokenizer::default())
                    .filter(StopWordFilter::new(language).unwrap())
                    .filter(RemoveLongFilter::limit(40))
                    .build()
            } else if config.stemming() {
                TextAnalyzer::builder(SimpleTokenizer::default())
                    .filter(Stemmer::new(language))
                    .filter(RemoveLongFilter::limit(40))
                    .build()
            } else {
                TextAnalyzer::builder(SimpleTokenizer::default())
                    .filter(RemoveLongFilter::limit(40))
                    .build()
            }
        } else {
            // Case-insensitive path (most common)
            if config.remove_stopwords() && config.stemming() {
                TextAnalyzer::builder(SimpleTokenizer::default())
                    .filter(LowerCaser)
                    .filter(StopWordFilter::new(language).unwrap())
                    .filter(Stemmer::new(language))
                    .filter(RemoveLongFilter::limit(40))
                    .build()
            } else if config.remove_stopwords() {
                TextAnalyzer::builder(SimpleTokenizer::default())
                    .filter(LowerCaser)
                    .filter(StopWordFilter::new(language).unwrap())
                    .filter(RemoveLongFilter::limit(40))
                    .build()
            } else if config.stemming() {
                TextAnalyzer::builder(SimpleTokenizer::default())
                    .filter(LowerCaser)
                    .filter(Stemmer::new(language))
                    .filter(RemoveLongFilter::limit(40))
                    .build()
            } else {
                TextAnalyzer::builder(SimpleTokenizer::default())
                    .filter(LowerCaser)
                    .filter(RemoveLongFilter::limit(40))
                    .build()
            }
        };

        // Register the analyzer
        index.tokenizers().register(&analyzer_name, analyzer);

        Ok(analyzer_name)
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
