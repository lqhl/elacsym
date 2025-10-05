//! Index management
//!
//! Handles vector indexes (RaBitQ) and full-text indexes (Tantivy)

pub mod fulltext;
pub mod vector;

pub use fulltext::FullTextIndex;
pub use vector::VectorIndex;
