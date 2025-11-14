//! elacsym-core: Core data structures and types for the elacsym vector database

pub mod types;
pub mod error;
pub mod vector;
pub mod slab;
pub mod namespace;

pub use error::{Error, Result};
pub use types::*;
pub use vector::{Vector, VectorId, Metadata, MetadataValue};
pub use slab::{Slab, SlabId, SlabMetadata};
pub use namespace::Namespace;
