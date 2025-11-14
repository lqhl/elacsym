//! Write buffer for collecting recent writes

use elacsym_core::{Result, Vector, VectorId};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Buffer for collecting recent writes before indexing
pub struct WriteBuffer {
    /// Buffered vectors
    vectors: Arc<RwLock<HashMap<VectorId, Vector>>>,

    /// Maximum buffer size
    max_size: usize,
}

impl WriteBuffer {
    pub fn new(max_size: usize) -> Self {
        Self {
            vectors: Arc::new(RwLock::new(HashMap::new())),
            max_size,
        }
    }

    /// Add a vector to the buffer
    pub fn add(&self, vector: Vector) -> Result<()> {
        let mut vectors = self.vectors.write();
        vectors.insert(vector.id.clone(), vector);
        Ok(())
    }

    /// Add multiple vectors to the buffer
    pub fn add_batch(&self, batch: Vec<Vector>) -> Result<()> {
        let mut vectors = self.vectors.write();
        for vector in batch {
            vectors.insert(vector.id.clone(), vector);
        }
        Ok(())
    }

    /// Get a vector by ID
    pub fn get(&self, id: &VectorId) -> Option<Vector> {
        let vectors = self.vectors.read();
        vectors.get(id).cloned()
    }

    /// Remove a vector
    pub fn remove(&self, id: &VectorId) -> Option<Vector> {
        let mut vectors = self.vectors.write();
        vectors.remove(id)
    }

    /// Get all vectors and clear the buffer
    pub fn drain(&self) -> Vec<Vector> {
        let mut vectors = self.vectors.write();
        let result: Vec<Vector> = vectors.values().cloned().collect();
        vectors.clear();
        result
    }

    /// Get current buffer size
    pub fn len(&self) -> usize {
        self.vectors.read().len()
    }

    /// Check if buffer is empty
    pub fn is_empty(&self) -> bool {
        self.vectors.read().is_empty()
    }

    /// Check if buffer should be flushed
    pub fn should_flush(&self) -> bool {
        self.len() >= self.max_size
    }

    /// Get all vectors without clearing
    pub fn get_all(&self) -> Vec<Vector> {
        self.vectors.read().values().cloned().collect()
    }

    /// Clear the buffer
    pub fn clear(&self) {
        self.vectors.write().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use elacsym_core::Metadata;

    #[test]
    fn test_write_buffer() {
        let buffer = WriteBuffer::new(100);

        let vector = Vector::new(
            VectorId::from("v1"),
            vec![1.0, 2.0, 3.0],
            Metadata::new(),
            "default",
        );

        buffer.add(vector.clone()).unwrap();
        assert_eq!(buffer.len(), 1);

        let retrieved = buffer.get(&VectorId::from("v1"));
        assert!(retrieved.is_some());

        let drained = buffer.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(buffer.len(), 0);
    }
}
