//! Tantivy-backed full-text search utilities.

use std::collections::HashMap;

use anyhow::{anyhow, Context, Result};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{
    self, IndexRecordOption, Schema, SchemaBuilder, TextFieldIndexing, TextOptions, STRING,
};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy};

/// Declarative configuration for a Tantivy-backed FTS schema.
#[derive(Clone)]
pub struct SchemaConfig {
    id_field_name: String,
    text_fields: Vec<TextFieldConfig>,
    tokenizers: Vec<(String, tantivy::tokenizer::TextAnalyzer)>,
}

impl SchemaConfig {
    /// Creates a new schema configuration with the given identifier field name.
    pub fn new(id_field_name: impl Into<String>) -> Self {
        Self {
            id_field_name: id_field_name.into(),
            text_fields: Vec::new(),
            tokenizers: Vec::new(),
        }
    }

    /// Registers a text field that should be indexed within the schema.
    pub fn add_text_field(mut self, field: TextFieldConfig) -> Self {
        self.text_fields.push(field);
        self
    }

    /// Registers a tokenizer available to all text fields.
    pub fn register_tokenizer(
        mut self,
        name: impl Into<String>,
        analyzer: tantivy::tokenizer::TextAnalyzer,
    ) -> Self {
        self.tokenizers.push((name.into(), analyzer));
        self
    }
}

/// Configuration for a single text field inside the Tantivy schema.
#[derive(Clone, Debug)]
pub struct TextFieldConfig {
    name: String,
    stored: bool,
    tokenizer: Option<String>,
    index_option: IndexRecordOption,
    field_boost: f32,
}

impl TextFieldConfig {
    /// Creates a new text field configuration using Tantivy's default tokenizer.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            stored: false,
            tokenizer: None,
            index_option: IndexRecordOption::WithFreqsAndPositions,
            field_boost: 1.0,
        }
    }

    /// Marks the field as stored, making it retrievable with search hits.
    pub fn stored(mut self) -> Self {
        self.stored = true;
        self
    }

    /// Overrides the tokenizer used for the field.
    pub fn with_tokenizer(mut self, tokenizer: impl Into<String>) -> Self {
        self.tokenizer = Some(tokenizer.into());
        self
    }

    /// Sets the index record option (positions/frequencies) used for BM25 scoring.
    pub fn with_index_option(mut self, option: IndexRecordOption) -> Self {
        self.index_option = option;
        self
    }

    /// Adjusts how strongly the field influences query parsing.
    pub fn with_boost(mut self, boost: f32) -> Self {
        self.field_boost = boost;
        self
    }
}

#[derive(Debug, Clone)]
struct TextFieldHandle {
    field: schema::Field,
    boost: f32,
}

/// Wrapper around Tantivy's [`Index`] tailored for elacsym's BM25 flows.
#[derive(Debug)]
pub struct TantivyIndex {
    index: Index,
    schema: Schema,
    id_field: schema::Field,
    fields_by_name: HashMap<String, schema::Field>,
    text_fields: Vec<TextFieldHandle>,
}

impl TantivyIndex {
    /// Builds a Tantivy index stored entirely in RAM.
    pub fn create_in_ram(config: SchemaConfig) -> Result<Self> {
        Self::create_with_builder(config, |schema| Ok(Index::create_in_ram(schema)))
    }

    fn create_with_builder<F>(config: SchemaConfig, index_builder: F) -> Result<Self>
    where
        F: FnOnce(Schema) -> Result<Index>,
    {
        let SchemaConfig {
            id_field_name,
            text_fields,
            tokenizers,
        } = config;

        if text_fields.is_empty() {
            return Err(anyhow!("schema must declare at least one text field"));
        }

        let mut builder = SchemaBuilder::default();

        let mut fields_by_name = HashMap::new();
        let mut text_handles = Vec::with_capacity(text_fields.len());

        let mut id_options = STRING;
        id_options = id_options.set_stored();
        let id_field = builder.add_text_field(&id_field_name, id_options);
        fields_by_name.insert(id_field_name.clone(), id_field);

        for field in &text_fields {
            let mut options = TextOptions::default();
            let indexing = TextFieldIndexing::default()
                .set_tokenizer(field.tokenizer.as_deref().unwrap_or("default"))
                .set_index_option(field.index_option);
            options = options.set_indexing_options(indexing);
            if field.stored {
                options = options.set_stored();
            }

            let schema_field = builder.add_text_field(&field.name, options);
            fields_by_name.insert(field.name.clone(), schema_field);
            text_handles.push(TextFieldHandle {
                field: schema_field,
                boost: field.field_boost,
            });
        }

        let schema = builder.build();
        let index = index_builder(schema.clone())?;

        for (tokenizer_name, analyzer) in tokenizers {
            index.tokenizers().register(&tokenizer_name, analyzer);
        }

        Ok(Self {
            index,
            schema,
            id_field,
            fields_by_name,
            text_fields: text_handles,
        })
    }

    /// Returns a reference to the inner Tantivy index.
    pub fn index(&self) -> &Index {
        &self.index
    }

    /// Returns the schema backing the index.
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Retrieves the identifier field.
    pub fn id_field(&self) -> schema::Field {
        self.id_field
    }

    /// Finds a field by name if present in the schema.
    pub fn field(&self, name: &str) -> Option<schema::Field> {
        self.fields_by_name.get(name).copied()
    }

    /// Opens an [`IndexWriter`] with the requested heap size.
    pub fn index_writer(&self, heap_size_bytes: usize) -> Result<IndexWriter> {
        self.index
            .writer(heap_size_bytes)
            .context("failed to create Tantivy index writer")
    }

    /// Creates an [`IndexReader`] using `ReloadPolicy::OnCommit`.
    pub fn reader(&self) -> Result<IndexReader> {
        self.index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommit)
            .try_into()
            .context("failed to open Tantivy index reader")
    }

    /// Executes a BM25 query and returns scored document identifiers.
    pub fn search(
        &self,
        reader: &IndexReader,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<SearchHit>> {
        if top_k == 0 {
            return Ok(Vec::new());
        }

        let searcher = reader.searcher();
        let default_fields: Vec<_> = self.text_fields.iter().map(|f| f.field).collect();
        let mut parser = QueryParser::for_index(&self.index, default_fields);
        for field in &self.text_fields {
            if (field.boost - 1.0).abs() > f32::EPSILON {
                parser.set_field_boost(field.field, field.boost);
            }
        }

        let parsed = parser
            .parse_query(query)
            .with_context(|| format!("failed to parse Tantivy query: {query}"))?;

        let top_docs = searcher
            .search(&parsed, &TopDocs::with_limit(top_k))
            .context("Tantivy search execution failed")?;

        let mut hits = Vec::with_capacity(top_docs.len());
        for (score, doc_address) in top_docs {
            let doc = searcher
                .doc(doc_address)
                .context("failed to load document for BM25 hit")?;
            let id_value = doc
                .get_first(self.id_field)
                .and_then(|value| value.as_text())
                .ok_or_else(|| anyhow!("document is missing the stored id field"))?;
            hits.push(SearchHit {
                score,
                doc_id: id_value.to_string(),
            });
        }

        Ok(hits)
    }
}

/// Minimal view of a Tantivy search hit that the planner consumes.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    /// BM25 score returned by Tantivy.
    pub score: f32,
    /// Identifier stored alongside the document.
    pub doc_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tantivy::doc;

    #[test]
    fn bm25_search_prefers_strong_match() -> Result<()> {
        let config = SchemaConfig::new("doc_id")
            .add_text_field(TextFieldConfig::new("title").stored())
            .add_text_field(TextFieldConfig::new("body"));
        let index = TantivyIndex::create_in_ram(config)?;

        let id_field = index.id_field();
        let title_field = index.field("title").expect("title field");
        let body_field = index.field("body").expect("body field");

        let mut writer = index.index_writer(50_000_000)?;
        writer.add_document(doc!(
            id_field => "doc-1",
            title_field => "Learning Tantivy search",
            body_field => "Tantivy is a rust search engine library for full-text search"
        ))?;
        writer.add_document(doc!(
            id_field => "doc-2",
            title_field => "Vector databases",
            body_field => "hybrid search blends vector and full text retrieval"
        ))?;
        writer.commit()?;

        let reader = index.reader()?;
        reader.reload()?;

        let hits = index.search(&reader, "rust search engine", 1)?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc_id, "doc-1");

        Ok(())
    }

    #[test]
    fn boosted_field_affects_ranking() -> Result<()> {
        let config = SchemaConfig::new("doc_id")
            .add_text_field(TextFieldConfig::new("title").stored().with_boost(3.0))
            .add_text_field(TextFieldConfig::new("body"));
        let index = TantivyIndex::create_in_ram(config)?;

        let id_field = index.id_field();
        let title_field = index.field("title").expect("title field");
        let body_field = index.field("body").expect("body field");

        let mut writer = index.index_writer(50_000_000)?;
        writer.add_document(doc!(
            id_field => "doc-a",
            title_field => "Hybrid search primer",
            body_field => "BM25 integration details"
        ))?;
        writer.add_document(doc!(
            id_field => "doc-b",
            title_field => "BM25 deep dive",
            body_field => "Comprehensive guide to BM25 search"
        ))?;
        writer.commit()?;

        let reader = index.reader()?;
        reader.reload()?;

        let hits = index.search(&reader, "BM25 search", 2)?;
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].doc_id, "doc-b");
        assert!(hits[0].score >= hits[1].score);

        Ok(())
    }
}
