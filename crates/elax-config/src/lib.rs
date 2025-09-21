use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{anyhow, Context, Result};
use object_store::{
    aws::AmazonS3Builder, gcp::GoogleCloudStorageBuilder, local::LocalFileSystem, ObjectStore,
};
use serde::Deserialize;

/// Default on-disk data root when no configuration is provided.
fn default_data_root() -> PathBuf {
    PathBuf::from(".elacsym")
}

/// Shared service configuration covering data roots and object-store wiring.
#[derive(Debug, Clone, Deserialize)]
pub struct ServiceConfig {
    /// Local filesystem root for WAL/router state.
    #[serde(default = "default_data_root")]
    pub data_root: PathBuf,
    /// Object-store wiring for part and segment assets.
    #[serde(default)]
    pub object_store: ObjectStoreConfig,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            data_root: default_data_root(),
            object_store: ObjectStoreConfig::default(),
        }
    }
}

impl ServiceConfig {
    /// Load configuration from a TOML file, applying environment overrides where present.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let mut config = if path.exists() {
            let contents = fs::read_to_string(path)
                .with_context(|| format!("reading config file: {:?}", path))?;
            toml::from_str::<ServiceConfig>(&contents)
                .with_context(|| format!("parsing config file: {:?}", path))?
        } else {
            ServiceConfig::default()
        };
        config.apply_env_overrides()?;
        config
            .object_store
            .resolve_filesystem_root(&config.data_root);
        Ok(config)
    }

    fn apply_env_overrides(&mut self) -> Result<()> {
        if let Ok(root) = std::env::var("ELAX_DATA_ROOT") {
            self.data_root = PathBuf::from(root);
        }
        if let Ok(kind) = std::env::var("ELAX_OBJECT_STORE_KIND") {
            self.object_store.kind = parse_kind(&kind)?;
        }
        if let Ok(bucket) = std::env::var("ELAX_OBJECT_STORE_BUCKET") {
            self.object_store.bucket = Some(bucket);
        }
        if let Ok(prefix) = std::env::var("ELAX_OBJECT_STORE_PREFIX") {
            self.object_store.prefix = Some(prefix);
        }
        if let Ok(endpoint) = std::env::var("ELAX_OBJECT_STORE_ENDPOINT") {
            self.object_store.endpoint = Some(endpoint);
        }
        if let Ok(region) = std::env::var("ELAX_OBJECT_STORE_REGION") {
            self.object_store.region = Some(region);
        }
        if let Ok(path) = std::env::var("ELAX_OBJECT_STORE_PATH") {
            self.object_store.root = Some(PathBuf::from(path));
        }
        Ok(())
    }
}

/// Supported object-store backends.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectStoreKind {
    /// Amazon S3-compatible object stores.
    S3,
    /// Google Cloud Storage-compatible object stores.
    Gcs,
    /// Filesystem-backed object store (primarily for tests and local bring-up).
    Filesystem,
}

impl Default for ObjectStoreKind {
    fn default() -> Self {
        Self::Filesystem
    }
}

/// Configuration for provisioning an [`object_store::ObjectStore`] client.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ObjectStoreConfig {
    #[serde(default)]
    pub kind: ObjectStoreKind,
    #[serde(default)]
    pub bucket: Option<String>,
    #[serde(default)]
    pub root: Option<PathBuf>,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
}

impl ObjectStoreConfig {
    /// Build an object-store handle with a normalized prefix for namespace usage.
    pub fn build(&self) -> Result<ObjectStoreHandle> {
        let prefix = normalize_prefix(self.prefix.as_deref());
        match self.kind {
            ObjectStoreKind::Filesystem => {
                let root = self
                    .root
                    .clone()
                    .context("filesystem object store requires `root` path")?;
                let fs = LocalFileSystem::new_with_prefix(root)
                    .context("configuring filesystem object store")?;
                Ok(ObjectStoreHandle {
                    store: Arc::new(fs),
                    prefix,
                })
            }
            ObjectStoreKind::S3 => {
                let bucket = self
                    .bucket
                    .as_ref()
                    .context("s3 object store requires `bucket`")?;
                let mut builder = AmazonS3Builder::new().with_bucket_name(bucket);
                if let Some(region) = &self.region {
                    builder = builder.with_region(region);
                }
                if let Some(endpoint) = &self.endpoint {
                    builder = builder.with_endpoint(endpoint);
                }
                let store = builder.build().context("building s3 object store client")?;
                Ok(ObjectStoreHandle {
                    store: Arc::new(store),
                    prefix,
                })
            }
            ObjectStoreKind::Gcs => {
                let bucket = self
                    .bucket
                    .as_ref()
                    .context("gcs object store requires `bucket`")?;
                let mut builder = GoogleCloudStorageBuilder::new().with_bucket_name(bucket);
                if let Some(endpoint) = &self.endpoint {
                    builder = builder.with_url(endpoint);
                }
                let store = builder
                    .build()
                    .context("building gcs object store client")?;
                Ok(ObjectStoreHandle {
                    store: Arc::new(store),
                    prefix,
                })
            }
        }
    }

    /// Populate filesystem roots when not explicitly configured.
    pub fn resolve_filesystem_root(&mut self, default: &Path) {
        if matches!(self.kind, ObjectStoreKind::Filesystem) && self.root.is_none() {
            self.root = Some(default.to_path_buf());
        }
    }
}

/// Handle containing a configured object store and namespace prefix.
#[derive(Clone)]
pub struct ObjectStoreHandle {
    pub store: Arc<dyn ObjectStore>,
    pub prefix: String,
}

impl std::fmt::Debug for ObjectStoreHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectStoreHandle")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

fn parse_kind(value: &str) -> Result<ObjectStoreKind> {
    match value.to_ascii_lowercase().as_str() {
        "s3" => Ok(ObjectStoreKind::S3),
        "gcs" => Ok(ObjectStoreKind::Gcs),
        "filesystem" | "fs" => Ok(ObjectStoreKind::Filesystem),
        other => Err(anyhow!("unsupported object store kind: {other}")),
    }
}

fn normalize_prefix(value: Option<&str>) -> String {
    value
        .unwrap_or("")
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_prefix_components() {
        assert_eq!(normalize_prefix(Some("")), "");
        assert_eq!(normalize_prefix(Some("/foo/bar/")), "foo/bar");
        assert_eq!(normalize_prefix(Some("foo//bar")), "foo/bar");
    }

    #[test]
    fn parse_kind_env_override() {
        let kind = parse_kind("S3").unwrap();
        assert!(matches!(kind, ObjectStoreKind::S3));
    }

    #[test]
    fn fills_filesystem_root_from_default() {
        let mut config = ObjectStoreConfig {
            kind: ObjectStoreKind::Filesystem,
            ..Default::default()
        };
        config.resolve_filesystem_root(Path::new("/tmp/test-root"));
        assert_eq!(config.root.as_deref(), Some(Path::new("/tmp/test-root")));
    }
}
