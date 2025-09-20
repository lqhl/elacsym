//! Full-text search scaffolding for BM25 and tokenization.

use anyhow::Result;

/// Placeholder tokenizer returning token count.
pub fn tokenize(_text: &str) -> Result<usize> {
    Ok(0)
}
