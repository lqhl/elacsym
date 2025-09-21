//! Tantivy-backed full-text search utilities.

use std::collections::HashMap;

use anyhow::{anyhow, Context, Result};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{
    self, IndexRecordOption, Schema, SchemaBuilder, TextFieldIndexing, TextOptions, STRING,
};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy};

mod directory;
mod language;

pub use directory::ObjectStoreDirectory;
pub use language::{FtsLanguage, LanguageOptions, LanguagePack, LanguagePackConfig};

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

    /// Registers a [`LanguagePack`] analyzer and makes it available to schema fields.
    pub fn register_language_pack(mut self, pack: LanguagePack) -> Self {
        let (name, analyzer) = pack.into_named_analyzer();
        if !self
            .tokenizers
            .iter()
            .any(|(existing, _)| existing == &name)
        {
            self.tokenizers.push((name, analyzer));
        }
        self
    }

    /// Registers a [`LanguagePackConfig`] by materializing its analyzer.
    pub fn register_language_pack_config(self, config: LanguagePackConfig) -> Self {
        self.register_language_pack(config.into())
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

    /// Assigns the tokenizer registered by the provided [`LanguagePack`].
    pub fn with_language(mut self, pack: &LanguagePack) -> Self {
        self.tokenizer = Some(pack.tokenizer_name().to_string());
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
    use tantivy::{doc, tokenizer::TextAnalyzer};

    fn collect_tokens(mut analyzer: TextAnalyzer, text: &str) -> Vec<String> {
        let mut stream = analyzer.token_stream(text);
        let mut tokens = Vec::new();
        while stream.advance() {
            tokens.push(stream.token().text.clone());
        }
        tokens
    }

    #[test]
    fn schema_config_registers_language_pack() -> Result<()> {
        let french = LanguagePack::new(FtsLanguage::French);
        let field_pack = french.clone();

        let config = SchemaConfig::new("doc_id")
            .register_language_pack(french)
            .add_text_field(TextFieldConfig::new("body").with_language(&field_pack));
        let index = TantivyIndex::create_in_ram(config)?;

        let mut analyzer = index
            .index()
            .tokenizers()
            .get(field_pack.tokenizer_name())
            .expect("tokenizer registered");
        let mut stream = analyzer.token_stream("Déjà vu dans les forêts");
        let mut tokens = Vec::new();
        while stream.advance() {
            tokens.push(stream.token().text.clone());
        }

        // Lower casing and ASCII folding normalize accents, stemming trims suffixes,
        // and stop words remove common fillers such as "les" and "dans".
        assert_eq!(tokens, vec!["dej", "vu", "foret"]);

        Ok(())
    }

    #[test]
    fn schema_config_registers_language_pack_config() -> Result<()> {
        let config = LanguagePackConfig {
            language: FtsLanguage::Portuguese,
            name: Some("pt_custom".to_string()),
            stemming: Some(false),
            stop_words: None,
            ascii_folding: Some(false),
            lower_case: Some(false),
            remove_long_limit: Some(Some(60)),
        };

        let pack: LanguagePack = config.clone().into();
        assert_eq!(pack.tokenizer_name(), "pt_custom");
        assert!(!pack.options().stemming);
        assert!(!pack.options().ascii_folding);
        assert!(!pack.options().lower_case);
        assert_eq!(pack.options().remove_long_limit, Some(60));

        let schema = SchemaConfig::new("doc_id")
            .register_language_pack_config(config)
            .add_text_field(TextFieldConfig::new("body").with_tokenizer("pt_custom"));
        let index = TantivyIndex::create_in_ram(schema)?;

        let analyzer = index
            .index()
            .tokenizers()
            .get("pt_custom")
            .expect("tokenizer registered");
        let tokens = collect_tokens(analyzer, "Programação avançada");
        assert!(tokens.contains(&"Programação".to_string()));

        Ok(())
    }

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
