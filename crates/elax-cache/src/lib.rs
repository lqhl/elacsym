//! NVMe and RAM cache manager scaffolding.

use anyhow::Result;
use parking_lot::Mutex;

/// Placeholder cache container storing a byte counter.
pub struct Cache {
    bytes: Mutex<usize>,
}

impl Cache {
    pub fn new() -> Self {
        Self {
            bytes: Mutex::new(0),
        }
    }

    /// Pretend to reserve space in the cache.
    pub fn reserve(&self, amount: usize) -> Result<()> {
        let mut guard = self.bytes.lock();
        *guard += amount;
        Ok(())
    }
}
