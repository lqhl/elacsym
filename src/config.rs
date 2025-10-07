use std::env;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::namespace::WalConfig;
use crate::storage::StorageConfig;

const DEFAULT_CACHE_MEMORY_BYTES: usize = 4 * 1024 * 1024 * 1024; // 4 GiB
const DEFAULT_CACHE_DISK_BYTES: usize = 100 * 1024 * 1024 * 1024; // 100 GiB

/// Top-level application configuration loaded from file + environment.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub storage: StorageSection,
    pub cache: CacheSection,
    pub compaction: CompactionSection,
    pub logging: LoggingSection,
    pub distributed: Option<DistributedSection>,
}

impl AppConfig {
    /// Load configuration from disk and environment.
    pub fn load() -> Result<Self> {
        let config_path = env::var("ELACSYM_CONFIG").unwrap_or_else(|_| "config.toml".to_string());

        let mut builder = config::Config::builder();

        if Path::new(&config_path).exists() {
            builder = builder.add_source(config::File::from(PathBuf::from(&config_path)));
        }

        builder = builder.add_source(
            config::Environment::with_prefix("ELACSYM")
                .separator("_")
                .try_parsing(true),
        );

        let settings = builder.build()?;
        let mut config: Self = settings.try_deserialize()?;

        // Ensure distributed section exists if node-specific overrides were provided
        if config.distributed.is_none()
            && (env::var("ELACSYM_NODE_ID").is_ok() || env::var("ELACSYM_NODE_ROLE").is_ok())
        {
            config.distributed = Some(DistributedSection::default());
        }

        if let Some(dist) = config.distributed.as_mut() {
            if dist.node_id.is_none() {
                if let Ok(node_id) = env::var("ELACSYM_NODE_ID") {
                    dist.node_id = Some(node_id);
                }
            }
            if dist.role.is_none() {
                if let Ok(role) = env::var("ELACSYM_NODE_ROLE") {
                    dist.role = Some(role.parse().context("invalid ELACSYM_NODE_ROLE")?);
                }
            }
        }

        if config.logging.level.trim().is_empty() {
            config.logging.level = "info".to_string();
        }

        Ok(config)
    }

    /// Resolve storage configuration and associated WAL configuration.
    pub fn storage_runtime(&self) -> Result<(StorageConfig, WalConfig)> {
        let (storage_config, wal_config) = self.storage.to_runtime()?;

        if let Some(dist) = &self.distributed {
            if dist.enabled && !matches!(self.storage.backend, StorageBackendKind::S3) {
                bail!("Distributed mode requires S3 storage backend");
            }
        }

        Ok((storage_config, wal_config))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 3000,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StorageSection {
    pub backend: StorageBackendKind,
    pub local: Option<LocalStorageSection>,
    pub s3: Option<S3StorageSection>,
}

impl StorageSection {
    pub fn to_runtime(&self) -> Result<(StorageConfig, WalConfig)> {
        match self.backend {
            StorageBackendKind::Local => {
                let local = self.local.clone().unwrap_or_default();

                let root_path = local.root_path;
                let storage_config = StorageConfig::Local {
                    root_path: root_path.clone(),
                };
                let wal_path = PathBuf::from(&root_path).join("wal");
                Ok((storage_config, WalConfig::local(wal_path)))
            }
            StorageBackendKind::S3 => {
                let s3 = self
                    .s3
                    .clone()
                    .context("storage.s3 configuration required when backend is 's3'")?;

                if s3.bucket.trim().is_empty() {
                    bail!("storage.s3.bucket must be specified");
                }
                if s3.region.trim().is_empty() {
                    bail!("storage.s3.region must be specified");
                }

                let storage_config = StorageConfig::S3 {
                    bucket: s3.bucket,
                    region: s3.region,
                    endpoint: s3.endpoint,
                };
                Ok((
                    storage_config,
                    WalConfig::s3(s3.wal_prefix.and_then(|p| {
                        let trimmed = p.trim();
                        if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed.to_string())
                        }
                    })),
                ))
            }
        }
    }
}

impl Default for StorageSection {
    fn default() -> Self {
        Self {
            backend: StorageBackendKind::Local,
            local: Some(LocalStorageSection::default()),
            s3: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StorageBackendKind {
    #[default]
    Local,
    S3,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LocalStorageSection {
    pub root_path: String,
}

impl Default for LocalStorageSection {
    fn default() -> Self {
        Self {
            root_path: "./data".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct S3StorageSection {
    pub bucket: String,
    pub region: String,
    pub endpoint: Option<String>,
    pub wal_prefix: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CacheSection {
    pub memory_size: usize,
    pub disk_size: usize,
    pub disk_path: String,
}

impl Default for CacheSection {
    fn default() -> Self {
        Self {
            memory_size: DEFAULT_CACHE_MEMORY_BYTES,
            disk_size: DEFAULT_CACHE_DISK_BYTES,
            disk_path: "./cache".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CompactionSection {
    pub enabled: bool,
    pub interval_secs: u64,
    pub max_segments: usize,
    pub max_total_docs: usize,
}

impl Default for CompactionSection {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: 3600,
            max_segments: 100,
            max_total_docs: 1_000_000,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct LoggingSection {
    pub level: String,
    pub format: LogFormat,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Json,
    Text,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct DistributedSection {
    pub enabled: bool,
    pub node_id: Option<String>,
    pub role: Option<DistributedRole>,
    #[serde(rename = "indexer_cluster")]
    pub indexer: Option<IndexerClusterSection>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DistributedRole {
    Indexer,
    Query,
}

impl std::str::FromStr for DistributedRole {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "indexer" => Ok(DistributedRole::Indexer),
            "query" => Ok(DistributedRole::Query),
            other => anyhow::bail!("unsupported node role: {}", other),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct IndexerClusterSection {
    pub nodes: Vec<String>,
}
