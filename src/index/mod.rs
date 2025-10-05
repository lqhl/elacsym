//! Index management
//!
//! Handles vector indexes (RaBitQ) and full-text indexes (Tantivy)

pub mod vector;
pub mod fulltext;

pub use vector::VectorIndex;
pub use fulltext::FullTextIndex;
