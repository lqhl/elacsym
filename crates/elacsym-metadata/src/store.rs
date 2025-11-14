//! Metadata storage implementations

use elacsym_core::{Error, Result};
use parking_lot::RwLock;
use rocksdb::{DB, Options};
use std::path::Path;
use std::sync::Arc;

/// Metadata store interface
pub trait MetadataStore: Send + Sync {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<()>;
    fn delete(&self, key: &[u8]) -> Result<()>;
    fn prefix_scan(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>>;
}

/// RocksDB-based metadata store
pub struct RocksDBStore {
    db: Arc<RwLock<DB>>,
}

impl RocksDBStore {
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let mut opts = Options::default();
        opts.create_if_missing(true);

        let db = DB::open(&opts, path)
            .map_err(|e| Error::Metadata(format!("Failed to open RocksDB: {}", e)))?;

        Ok(Self {
            db: Arc::new(RwLock::new(db)),
        })
    }
}

impl MetadataStore for RocksDBStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let db = self.db.read();
        db.get(key)
            .map_err(|e| Error::Metadata(format!("RocksDB get failed: {}", e)))
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        let db = self.db.write();
        db.put(key, value)
            .map_err(|e| Error::Metadata(format!("RocksDB put failed: {}", e)))
    }

    fn delete(&self, key: &[u8]) -> Result<()> {
        let db = self.db.write();
        db.delete(key)
            .map_err(|e| Error::Metadata(format!("RocksDB delete failed: {}", e)))
    }

    fn prefix_scan(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let db = self.db.read();
        let mut results = Vec::new();

        let iter = db.prefix_iterator(prefix);
        for item in iter {
            match item {
                Ok((key, value)) => {
                    if key.starts_with(prefix) {
                        results.push((key.to_vec(), value.to_vec()));
                    } else {
                        break;
                    }
                }
                Err(e) => {
                    return Err(Error::Metadata(format!("RocksDB iteration failed: {}", e)));
                }
            }
        }

        Ok(results)
    }
}
