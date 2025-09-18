#![allow(dead_code)]

//! Tombstone application utilities.

use anyhow::Result;
use common::{DeletePartMetadata, DocId};
use roaring::RoaringBitmap;

/// Representation of live documents for a given part after tombstones are applied.
#[derive(Debug, Clone, Default)]
pub struct LiveSet {
    bitmap: RoaringBitmap,
}

impl LiveSet {
    /// Build a live-set bitmap from delete parts affecting the specified doc-id range.
    pub fn from_deletes(_deletes: &[DeletePartMetadata], _range: (DocId, DocId)) -> Result<Self> {
        Ok(Self {
            bitmap: RoaringBitmap::new(),
        })
    }

    /// Returns true if the provided doc id is considered live.
    pub fn contains(&self, doc: DocId) -> bool {
        match u32::try_from(doc) {
            Ok(value) => !self.bitmap.contains(value),
            Err(_) => false,
        }
    }
}
