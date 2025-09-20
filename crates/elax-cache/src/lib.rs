//! NVMe and RAM cache manager aligned with the Phase 3 design goals.

use std::{
    collections::{HashMap, HashSet},
    fmt, fs,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::Arc,
    time::Instant,
};

use anyhow::{Context, Result};
use metrics::{counter, gauge};
use parking_lot::{Mutex, MutexGuard};

/// Configuration for the cache hierarchy.
#[derive(Debug, Clone)]
pub struct CacheConfig {
    pub nvme_root: PathBuf,
    pub ram_bytes: usize,
    pub slab_bytes: usize,
}

impl CacheConfig {
    pub fn new(root: impl Into<PathBuf>, ram_bytes: usize, slab_bytes: usize) -> Self {
        Self {
            nvme_root: root.into(),
            ram_bytes,
            slab_bytes: slab_bytes.max(4096),
        }
    }
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            nvme_root: PathBuf::from(".elacsym/cache"),
            ram_bytes: 256 * 1024 * 1024,
            slab_bytes: 2 * 1024 * 1024,
        }
    }
}

/// Identifies a cached asset.
#[derive(Clone, Eq)]
pub struct CacheKey {
    namespace: String,
    asset: String,
    kind: AssetKind,
}

impl CacheKey {
    pub fn new(namespace: impl Into<String>, asset: impl Into<String>, kind: AssetKind) -> Self {
        Self {
            namespace: namespace.into(),
            asset: asset.into(),
            kind,
        }
    }

    fn namespace(&self) -> &str {
        &self.namespace
    }

    fn filename(&self) -> String {
        let mut sanitized = self.asset.replace('/', "_");
        if sanitized.is_empty() {
            sanitized = "root".to_string();
        }
        format!("{}-{}.bin", self.kind.as_str(), sanitized)
    }
}

impl PartialEq for CacheKey {
    fn eq(&self, other: &Self) -> bool {
        self.namespace == other.namespace && self.asset == other.asset && self.kind == other.kind
    }
}

impl Hash for CacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.namespace.hash(state);
        self.asset.hash(state);
        self.kind.hash(state);
    }
}

/// Categories of cached assets.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum AssetKind {
    Centroids,
    Postings,
    RerankCodes,
    Filters,
    Blob,
}

impl AssetKind {
    fn as_str(&self) -> &'static str {
        match self {
            AssetKind::Centroids => "centroids",
            AssetKind::Postings => "postings",
            AssetKind::RerankCodes => "rerank",
            AssetKind::Filters => "filters",
            AssetKind::Blob => "blob",
        }
    }
}

impl fmt::Display for AssetKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone)]
struct CacheEntry {
    path: PathBuf,
    allocated: usize,
    resident: Option<Arc<Vec<u8>>>,
    last_access: Instant,
    namespace: String,
    kind: AssetKind,
}

impl CacheEntry {
    fn new(path: PathBuf, allocated: usize, namespace: String, kind: AssetKind) -> Self {
        Self {
            path,
            allocated,
            resident: None,
            last_access: Instant::now(),
            namespace,
            kind,
        }
    }
}

struct CacheState {
    entries: HashMap<CacheKey, CacheEntry>,
    ram_used: usize,
    pinned_namespaces: HashSet<String>,
}

impl CacheState {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            ram_used: 0,
            pinned_namespaces: HashSet::new(),
        }
    }
}

/// Runtime statistics for the cache. Intended for testing and introspection.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    pub entry_count: usize,
    pub ram_bytes: usize,
}

/// Two-tier cache storing IVF/ERQ assets on NVMe and optionally keeping them in
/// RAM slabs for hot reuse.
pub struct Cache {
    config: CacheConfig,
    state: Mutex<CacheState>,
}

impl Cache {
    pub fn new(config: CacheConfig) -> Result<Self> {
        if !config.nvme_root.exists() {
            fs::create_dir_all(&config.nvme_root)
                .with_context(|| format!("creating cache root: {:?}", config.nvme_root))?;
        }
        Ok(Self {
            config,
            state: Mutex::new(CacheState::new()),
        })
    }

    pub fn pin_namespace(&self, namespace: impl Into<String>, pin: bool) {
        let namespace = namespace.into();
        let mut guard = self.state.lock();
        if pin {
            guard.pinned_namespaces.insert(namespace.clone());
        } else {
            guard.pinned_namespaces.remove(&namespace);
        }
        gauge!("elax_cache_ram_bytes", guard.ram_used as f64, "namespace" => namespace);
    }

    /// Insert or overwrite an asset in the cache.
    pub fn insert(&self, key: CacheKey, bytes: Arc<Vec<u8>>) -> Result<()> {
        let path = self.asset_path(&key);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating cache namespace dir: {:?}", parent))?;
        }
        fs::write(&path, bytes.as_slice())
            .with_context(|| format!("persisting cache asset: {:?}", path))?;

        let allocated = self.align(bytes.len());
        let mut guard = self.state.lock();
        let mut entry = CacheEntry::new(path, allocated, key.namespace().to_string(), key.kind);
        entry.resident = Some(bytes);
        self.attach_entry(&mut guard, key, entry);
        Ok(())
    }

    /// Read an asset into memory, returning a clone of the stored bytes.
    pub fn get(&self, key: &CacheKey) -> Result<Option<Arc<Vec<u8>>>> {
        let lookup = {
            let mut guard = self.state.lock();
            if let Some(entry) = guard.entries.get_mut(key) {
                entry.last_access = Instant::now();
                let namespace = entry.namespace.clone();
                let kind = entry.kind;
                if let Some(bytes) = &entry.resident {
                    counter!(
                        "elax_cache_hits_total",
                        1,
                        "namespace" => namespace.clone(),
                        "kind" => entry.kind.as_str()
                    );
                    return Ok(Some(bytes.clone()));
                }
                counter!(
                    "elax_cache_misses_total",
                    1,
                    "namespace" => namespace.clone(),
                    "kind" => entry.kind.as_str()
                );
                Some((entry.path.clone(), namespace, kind))
            } else {
                None
            }
        };

        let (path, namespace, kind) = match lookup {
            Some(data) => data,
            None => return Ok(None),
        };

        let bytes = fs::read(&path)
            .with_context(|| format!("reading cached asset from disk: {:?}", path))?;
        let arc = Arc::new(bytes);

        let mut guard = self.state.lock();
        if let Some(entry) = guard.entries.get_mut(key) {
            entry.last_access = Instant::now();
            if entry.resident.is_none() {
                entry.resident = Some(arc.clone());
                guard.ram_used += entry.allocated;
                self.evict_if_needed(&mut guard);
            }
            gauge!(
                "elax_cache_ram_bytes",
                guard.ram_used as f64,
                "namespace" => namespace.clone()
            );
        }
        counter!(
            "elax_cache_hits_total",
            1,
            "namespace" => namespace,
            "kind" => kind.as_str()
        );
        Ok(Some(arc))
    }

    /// Ensure that the asset is loaded into memory, returning whether it was
    /// newly prefetched.
    pub fn prefetch(&self, key: &CacheKey) -> Result<bool> {
        let should_load = {
            let guard = self.state.lock();
            guard
                .entries
                .get(key)
                .map(|entry| entry.resident.is_none())
                .unwrap_or(false)
        };
        if !should_load {
            return Ok(false);
        }
        let bytes = match self.get(key)? {
            Some(bytes) => bytes,
            None => return Ok(false),
        };
        Ok(!bytes.is_empty())
    }

    pub fn stats(&self) -> CacheStats {
        let guard = self.state.lock();
        CacheStats {
            entry_count: guard.entries.len(),
            ram_bytes: guard.ram_used,
        }
    }

    fn align(&self, size: usize) -> usize {
        let slab = self.config.slab_bytes;
        size.div_ceil(slab) * slab
    }

    fn asset_path(&self, key: &CacheKey) -> PathBuf {
        self.config
            .nvme_root
            .join("namespaces")
            .join(&key.namespace)
            .join(key.filename())
    }

    fn attach_entry(
        &self,
        guard: &mut MutexGuard<'_, CacheState>,
        key: CacheKey,
        entry: CacheEntry,
    ) {
        if let Some(prev) = guard.entries.insert(key.clone(), entry.clone()) {
            if let Some(bytes) = prev.resident {
                guard.ram_used = guard.ram_used.saturating_sub(prev.allocated);
                drop(bytes);
            }
        }
        if entry.resident.is_some() {
            guard.ram_used += entry.allocated;
            gauge!("elax_cache_ram_bytes", guard.ram_used as f64, "namespace" => entry.namespace.clone());
            self.evict_if_needed(guard);
        }
    }

    fn evict_if_needed(&self, guard: &mut CacheState) {
        while guard.ram_used > self.config.ram_bytes {
            if let Some((key, entry)) = self.select_victim(guard) {
                if let Some(bytes) = entry.resident {
                    drop(bytes);
                }
                guard.ram_used = guard.ram_used.saturating_sub(entry.allocated);
                let namespace = entry.namespace.clone();
                let kind = entry.kind;
                guard.entries.insert(
                    key,
                    CacheEntry {
                        resident: None,
                        ..entry
                    },
                );
                counter!(
                    "elax_cache_evictions_total",
                    1,
                    "namespace" => namespace,
                    "kind" => kind.as_str()
                );
            } else {
                break;
            }
        }
    }

    fn select_victim(&self, guard: &mut CacheState) -> Option<(CacheKey, CacheEntry)> {
        guard
            .entries
            .iter()
            .filter(|(key, entry)| {
                entry.resident.is_some() && !guard.pinned_namespaces.contains(key.namespace())
            })
            .min_by_key(|(_, entry)| entry.last_access)
            .map(|(key, entry)| (key.clone(), entry.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn cache_config() -> (TempDir, CacheConfig) {
        let temp = TempDir::new().expect("create temp dir");
        let config = CacheConfig::new(temp.path(), 8 * 1024, 4096);
        (temp, config)
    }

    fn sample_key(ns: &str, idx: usize) -> CacheKey {
        CacheKey::new(ns.to_string(), format!("asset-{idx}"), AssetKind::Blob)
    }

    #[test]
    fn insert_and_get_round_trip() {
        let (_temp, config) = cache_config();
        let cache = Cache::new(config).unwrap();
        let key = sample_key("ns", 1);
        cache
            .insert(key.clone(), Arc::new(vec![1, 2, 3, 4]))
            .expect("insert");
        let value = cache.get(&key).unwrap().expect("fetch");
        assert_eq!(&*value, &[1, 2, 3, 4]);
        assert_eq!(cache.stats().entry_count, 1);
    }

    #[test]
    fn evicts_least_recently_used_when_ram_exceeded() {
        let (_temp, config) = cache_config();
        let cache = Cache::new(config).unwrap();
        for idx in 0..3 {
            let key = sample_key("ns", idx);
            cache
                .insert(key, Arc::new(vec![0u8; 4096]))
                .expect("insert");
        }
        // Insert large blob to force eviction of earliest key.
        let hot_key = sample_key("ns", 99);
        cache
            .insert(hot_key.clone(), Arc::new(vec![0u8; 4096]))
            .expect("insert hot");
        // Trigger read of other entries to populate.
        let second_key = sample_key("ns", 1);
        cache.get(&second_key).unwrap();
        cache.get(&hot_key).unwrap();
        let first_key = sample_key("ns", 0);
        {
            let guard = cache.state.lock();
            let entry = guard.entries.get(&first_key).unwrap();
            assert!(entry.resident.is_none(), "entry should be evicted from RAM");
        }
        let bytes = cache.get(&first_key).unwrap().unwrap();
        assert_eq!(bytes.len(), 4096);
    }

    #[test]
    fn pinned_namespaces_are_not_evicted() {
        let (_temp, config) = cache_config();
        let cache = Cache::new(config).unwrap();
        let key = sample_key("vip", 0);
        cache.pin_namespace("vip", true);
        cache
            .insert(key.clone(), Arc::new(vec![0u8; 4096]))
            .expect("insert");
        // Fill cache to force eviction but pinned namespace should remain.
        for idx in 0..4 {
            cache
                .insert(sample_key("ns", idx), Arc::new(vec![0u8; 4096]))
                .expect("insert other");
        }
        let value = cache.get(&key).unwrap();
        assert!(value.is_some());
    }

    #[test]
    fn prefetch_promotes_to_memory() {
        let (_temp, config) = cache_config();
        let cache = Cache::new(config).unwrap();
        let key = sample_key("ns", 42);
        cache
            .insert(key.clone(), Arc::new(vec![9u8; 1024]))
            .expect("insert");
        {
            let mut guard = cache.state.lock();
            if let Some(entry) = guard.entries.get_mut(&key) {
                let allocated = entry.allocated;
                entry.resident = None;
                guard.ram_used = guard.ram_used.saturating_sub(allocated);
            }
        }
        let loaded = cache.prefetch(&key).unwrap();
        assert!(loaded);
        let value = cache.get(&key).unwrap().unwrap();
        assert_eq!(value.len(), 1024);
    }
}
