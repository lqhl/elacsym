//! elacsym-config: Configuration management

use elacsym_core::DistanceMetric;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Main configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Server configuration
    #[serde(default)]
    pub server: ServerConfig,

    /// Storage configuration
    #[serde(default)]
    pub storage: StorageConfig,

    /// Metadata configuration
    #[serde(default)]
    pub metadata: MetadataConfig,

    /// Index configuration
    #[serde(default)]
    pub index: IndexConfig,

    /// Builder configuration
    #[serde(default)]
    pub builder: BuilderConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            storage: StorageConfig::default(),
            metadata: MetadataConfig::default(),
            index: IndexConfig::default(),
            builder: BuilderConfig::default(),
        }
    }
}

impl Config {
    /// Load configuration from a TOML file
    pub fn from_file<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    /// Load configuration from environment or default
    pub fn from_env() -> Self {
        if let Ok(path) = std::env::var("ELACSYM_CONFIG") {
            Self::from_file(path).unwrap_or_default()
        } else {
            Self::default()
        }
    }

    /// Save configuration to a TOML file
    pub fn to_file<P: AsRef<Path>>(&self, path: P) -> anyhow::Result<()> {
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }
}

/// Server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Server host
    pub host: String,

    /// Server port
    pub port: u16,

    /// Log level (trace, debug, info, warn, error)
    pub log_level: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 8080,
            log_level: "info".to_string(),
        }
    }
}

/// Storage configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Storage backend type (memory, local, s3)
    pub backend: String,

    /// Base path for slabs
    pub base_path: String,

    /// S3 bucket (if using S3)
    pub s3_bucket: Option<String>,

    /// S3 region (if using S3)
    pub s3_region: Option<String>,

    /// S3 endpoint (if using S3-compatible storage like MinIO)
    pub s3_endpoint: Option<String>,

    /// Local path (if using local storage)
    pub local_path: Option<String>,

    /// Cache configuration
    pub cache: CacheConfig,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: "memory".to_string(),
            base_path: "slabs".to_string(),
            s3_bucket: None,
            s3_region: None,
            s3_endpoint: None,
            local_path: None,
            cache: CacheConfig::default(),
        }
    }
}

/// Cache configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    /// Maximum cache size in bytes
    pub max_size_bytes: usize,

    /// Maximum number of slabs to cache
    pub max_slabs: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_size_bytes: 1024 * 1024 * 1024, // 1GB
            max_slabs: 1000,
        }
    }
}

/// Metadata configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataConfig {
    /// Metadata storage path
    pub path: String,
}

impl Default for MetadataConfig {
    fn default() -> Self {
        Self {
            path: "/tmp/elacsym_metadata".to_string(),
        }
    }
}

/// Index configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexConfig {
    /// Default vector dimension
    pub default_dimension: usize,

    /// Default distance metric (l2, inner_product, cosine)
    pub default_metric: String,

    /// Number of partitions (IVF clusters)
    pub nlist: usize,

    /// Total quantization bits
    pub total_bits: usize,

    /// Use faster config for large datasets
    pub use_faster_config: bool,

    /// Default nprobe for queries
    pub default_nprobe: usize,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            default_dimension: 128,
            default_metric: "l2".to_string(),
            nlist: 1024,
            total_bits: 6,
            use_faster_config: false,
            default_nprobe: 16,
        }
    }
}

impl IndexConfig {
    pub fn get_metric(&self) -> DistanceMetric {
        match self.default_metric.to_lowercase().as_str() {
            "l2" => DistanceMetric::L2,
            "inner_product" | "ip" => DistanceMetric::InnerProduct,
            "cosine" => DistanceMetric::Cosine,
            _ => DistanceMetric::L2,
        }
    }
}

/// Builder configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuilderConfig {
    /// Minimum vectors before building an index
    pub min_vectors: usize,

    /// Maximum vectors per slab
    pub max_vectors_per_slab: usize,

    /// Build interval in seconds
    pub build_interval_secs: u64,
}

impl Default for BuilderConfig {
    fn default() -> Self {
        Self {
            min_vectors: 1000,
            max_vectors_per_slab: 100_000,
            build_interval_secs: 60,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.server.port, 8080);
        assert_eq!(config.storage.backend, "memory");
        assert_eq!(config.index.default_dimension, 128);
    }

    #[test]
    fn test_config_serialization() {
        let config = Config::default();
        let toml_str = toml::to_string(&config).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.server.port, config.server.port);
    }
}
