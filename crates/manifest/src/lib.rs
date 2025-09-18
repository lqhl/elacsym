#![allow(dead_code)]

//! Helpers for reading and publishing manifest snapshots to S3.

use common::{Error, ManifestView, NamespaceConfig, Result};
use storage::ObjectStore;
use tracing::instrument;

/// Name of the object that stores the pointer to the latest manifest epoch.
pub const CURRENT_MANIFEST_KEY: &str = "manifest/current";

/// Fetches the namespace configuration and manifest for the provided namespace identifier.
#[instrument(skip(store))]
pub async fn load_manifest(store: &dyn ObjectStore, ns: &str) -> Result<ManifestView> {
    let _ = store.get(CURRENT_MANIFEST_KEY).await?;
    Err(Error::Message(format!(
        "manifest loading is not yet implemented for namespace {ns}"
    )))
}

/// Writes a new manifest epoch and atomically flips the current pointer.
#[instrument(skip(store, view))]
pub async fn publish_manifest(store: &dyn ObjectStore, view: &ManifestView) -> Result<()> {
    let _ = (store, view);
    Err(Error::Message(
        "manifest publication is not yet implemented".to_string(),
    ))
}

/// Validates a namespace configuration before it is persisted.
pub fn validate_namespace(cfg: &NamespaceConfig) -> Result<()> {
    if cfg.dim == 0 {
        return Err(Error::from("dimension must be positive"));
    }
    if !(0.0..=10.0).contains(&cfg.cluster_factor) {
        return Err(Error::from("cluster factor must be within a sane range"));
    }
    Ok(())
}
