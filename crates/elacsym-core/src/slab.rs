//! Slab (index segment) data structures

use crate::{PartitionId, VectorId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for a slab
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SlabId(String);

impl SlabId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn generate() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SlabId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Metadata about a slab
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlabMetadata {
    pub id: SlabId,
    pub namespace: String,
    pub partition_id: PartitionId,
    pub vector_count: usize,
    pub dimension: usize,
    pub created_at: DateTime<Utc>,
    pub size_bytes: usize,
    pub checksum: u32,
}

/// A slab represents an immutable index segment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Slab {
    /// Metadata
    pub metadata: SlabMetadata,

    /// Serialized RaBitQ index data
    #[serde(with = "serde_bytes")]
    pub index_data: Vec<u8>,

    /// Vector IDs in this slab (in order)
    pub vector_ids: Vec<VectorId>,
}

impl Slab {
    pub fn new(
        namespace: String,
        partition_id: PartitionId,
        dimension: usize,
        index_data: Vec<u8>,
        vector_ids: Vec<VectorId>,
    ) -> Self {
        let vector_count = vector_ids.len();
        let size_bytes = index_data.len();
        let checksum = crc32fast::hash(&index_data);

        let metadata = SlabMetadata {
            id: SlabId::generate(),
            namespace,
            partition_id,
            vector_count,
            dimension,
            created_at: Utc::now(),
            size_bytes,
            checksum,
        };

        Self {
            metadata,
            index_data,
            vector_ids,
        }
    }

    /// Verify checksum
    pub fn verify_checksum(&self) -> bool {
        let computed = crc32fast::hash(&self.index_data);
        computed == self.metadata.checksum
    }

    /// Get slab size in bytes
    pub fn size_bytes(&self) -> usize {
        self.metadata.size_bytes
    }

    /// Get number of vectors in this slab
    pub fn vector_count(&self) -> usize {
        self.metadata.vector_count
    }
}

// Add serde_bytes module for efficient byte serialization
mod serde_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(bytes)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        <Vec<u8>>::deserialize(deserializer)
    }
}
