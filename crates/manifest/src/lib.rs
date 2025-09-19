#![allow(dead_code)]

//! Helpers for reading and publishing manifest snapshots to S3.

use anyhow::Context;
use bytes::Bytes;
use common::{Epoch, Error, ManifestView, NamespaceConfig, Result};
use serde::{Deserialize, Serialize};
use storage::ObjectStore;
use tracing::instrument;

/// Name of the object that stores the pointer to the latest manifest epoch.
pub const CURRENT_MANIFEST_KEY: &str = "manifest/current";

/// Serialised representation of the manifest pointer stored under [`CURRENT_MANIFEST_KEY`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ManifestPointer {
    epoch: Epoch,
    etag: String,
}

/// Fetches the namespace configuration and manifest for the provided namespace identifier.
#[instrument(skip(store))]
pub async fn load_manifest(store: &dyn ObjectStore, ns: &str) -> Result<ManifestView> {
    let pointer_key = namespace_scoped(ns, CURRENT_MANIFEST_KEY);
    let pointer_obj = store
        .get(&pointer_key)
        .await
        .with_context(|| format!("failed to fetch manifest pointer for namespace {ns}"))?;

    let pointer: ManifestPointer = serde_json::from_slice(&pointer_obj.data)
        .with_context(|| format!("manifest pointer for namespace {ns} was malformed"))?;

    let manifest_key = manifest_epoch_key(ns, pointer.epoch);
    let manifest_obj = store.get(&manifest_key).await.with_context(|| {
        format!(
            "failed to fetch manifest epoch {} for namespace {ns}",
            pointer.epoch
        )
    })?;

    if let Some(etag) = manifest_obj.etag.as_deref() {
        if etag != pointer.etag {
            return Err(Error::Message(format!(
                "manifest etag mismatch for namespace {ns}: pointer expected {} but object reported {}",
                pointer.etag, etag
            )));
        }
    }

    let view: ManifestView = serde_json::from_slice(&manifest_obj.data)
        .with_context(|| format!("manifest epoch {} could not be parsed", pointer.epoch))?;

    if view.epoch != pointer.epoch {
        return Err(Error::Message(format!(
            "manifest epoch mismatch for namespace {ns}: pointer references {} but payload was {}",
            pointer.epoch, view.epoch
        )));
    }

    Ok(view)
}

/// Writes a new manifest epoch and atomically flips the current pointer.
#[instrument(skip(store, view))]
pub async fn publish_manifest(
    store: &dyn ObjectStore,
    ns: &str,
    view: &ManifestView,
) -> Result<()> {
    validate_namespace(&view.namespace)?;

    let pointer_key = namespace_scoped(ns, CURRENT_MANIFEST_KEY);
    let pointer_obj = store
        .get(&pointer_key)
        .await
        .with_context(|| format!("failed to fetch existing manifest pointer for namespace {ns}"))?;

    let current_pointer: ManifestPointer = serde_json::from_slice(&pointer_obj.data)
        .with_context(|| format!("manifest pointer for namespace {ns} was malformed"))?;

    if view.epoch <= current_pointer.epoch {
        return Err(Error::Message(format!(
            "refusing to publish stale manifest for namespace {ns}: {} <= {}",
            view.epoch, current_pointer.epoch
        )));
    }

    let manifest_key = manifest_epoch_key(ns, view.epoch);
    let manifest_bytes = serde_json::to_vec(view).map(Bytes::from).with_context(|| {
        format!(
            "failed to serialise manifest epoch {} for namespace {ns}",
            view.epoch
        )
    })?;
    let manifest_etag = store
        .put(&manifest_key, manifest_bytes)
        .await
        .with_context(|| {
            format!(
                "failed to persist manifest epoch {} for namespace {ns}",
                view.epoch
            )
        })?;

    let Some(pointer_etag) = pointer_obj.etag.as_deref() else {
        return Err(Error::Message(format!(
            "no etag was returned for manifest pointer of namespace {ns}; cannot perform conditional update"
        )));
    };

    let new_pointer = ManifestPointer {
        epoch: view.epoch,
        etag: manifest_etag,
    };
    let pointer_bytes = serde_json::to_vec(&new_pointer)
        .map(Bytes::from)
        .with_context(|| format!("failed to serialise manifest pointer for namespace {ns}"))?;

    store
        .put_if_match(&pointer_key, pointer_bytes, pointer_etag)
        .await
        .with_context(|| format!("failed to update manifest pointer for namespace {ns}"))?;

    Ok(())
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

fn namespace_scoped(ns: &str, suffix: &str) -> String {
    format!("namespaces/{ns}/{suffix}")
}

fn manifest_epoch_key(ns: &str, epoch: Epoch) -> String {
    namespace_scoped(ns, &format!("manifest/{epoch}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use bytes::Bytes;
    use common::{
        DeletePartId, DeletePartKind, DeletePartMetadata, DeletePartPaths, ManifestView,
        NamespaceConfig, NamespaceDefaults, PartId, PartMetadata, PartPaths, PartStatistics,
    };
    use std::collections::HashMap;
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    };
    use storage::ObjectBytes;
    use tokio::sync::Mutex;

    #[derive(Clone, Debug)]
    struct StoredObject {
        data: Bytes,
        etag: String,
    }

    #[derive(Clone, Default, Debug)]
    struct InMemoryStore {
        objects: Arc<Mutex<HashMap<String, StoredObject>>>,
        counter: Arc<AtomicU64>,
    }

    impl InMemoryStore {
        fn next_etag(&self) -> String {
            let value = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
            format!("\"etag-{value}\"")
        }

        async fn insert(&self, key: &str, data: Bytes) -> String {
            let etag = self.next_etag();
            let object = StoredObject {
                data,
                etag: etag.clone(),
            };
            self.objects.lock().await.insert(key.to_string(), object);
            etag
        }
    }

    #[async_trait]
    impl ObjectStore for InMemoryStore {
        async fn get(&self, key: &str) -> Result<ObjectBytes> {
            let guard = self.objects.lock().await;
            let object = guard
                .get(key)
                .cloned()
                .ok_or_else(|| Error::Message(format!("missing key {key}")))?;
            Ok(ObjectBytes::new(object.data, Some(object.etag)))
        }

        async fn put(&self, key: &str, data: Bytes) -> Result<String> {
            Ok(self.insert(key, data).await)
        }

        async fn put_if_match(&self, key: &str, data: Bytes, if_match: &str) -> Result<String> {
            let mut guard = self.objects.lock().await;
            let entry = guard
                .get_mut(key)
                .ok_or_else(|| Error::Message(format!("missing key {key}")))?;
            if entry.etag != if_match {
                return Err(Error::Message(format!(
                    "etag mismatch for key {key}: expected {}, found {if_match}",
                    entry.etag
                )));
            }
            entry.data = data;
            entry.etag = self.next_etag();
            Ok(entry.etag.clone())
        }
    }

    fn sample_manifest(epoch: Epoch) -> ManifestView {
        ManifestView {
            namespace: NamespaceConfig {
                dim: 128,
                cluster_factor: 1.0,
                k_min: 1,
                k_max: 1024,
                nprobe_cap: 16,
                defaults: NamespaceDefaults::recommended(),
            },
            parts: vec![PartMetadata {
                part_id: PartId("p1".to_string()),
                n: 10,
                dim: 128,
                k_trained: 4,
                small_part_fallback: false,
                doc_id_range: (0, 10),
                paths: PartPaths {
                    centroids: "parts/p1/ivf/centroids.bin".to_string(),
                    ilist_dir: "parts/p1/ivf/lists/".to_string(),
                    rabitq_meta: "parts/p1/rabitq/meta.json".to_string(),
                    rabitq_codes: "parts/p1/rabitq/codes-1bit.bin".to_string(),
                    vec_int8_dir: "parts/p1/vectors/int8/".to_string(),
                    vec_fp32_dir: "parts/p1/vectors/fp32/".to_string(),
                },
                stats: PartStatistics {
                    created_at: "2024-01-01T00:00:00Z".to_string(),
                    mean_norm: 1.0,
                },
            }],
            delete_parts: vec![DeletePartMetadata {
                del_part_id: DeletePartId("d1".to_string()),
                kind: DeletePartKind::Bitmap,
                created_at: "2024-01-02T00:00:00Z".to_string(),
                paths: DeletePartPaths {
                    bitmap: Some("deletes/d1/tombstone.bitmap.roaring".to_string()),
                    ids: None,
                },
            }],
            epoch,
        }
    }

    async fn seed_manifest(store: &InMemoryStore, ns: &str, view: &ManifestView) -> Result<()> {
        let manifest_key = manifest_epoch_key(ns, view.epoch);
        let manifest_bytes = serde_json::to_vec(view)
            .map(Bytes::from)
            .map_err(|err| Error::Context(err.into()))?;
        let manifest_etag = store.put(&manifest_key, manifest_bytes).await?;

        let pointer_key = namespace_scoped(ns, CURRENT_MANIFEST_KEY);
        let pointer_bytes = serde_json::to_vec(&ManifestPointer {
            epoch: view.epoch,
            etag: manifest_etag,
        })
        .map(Bytes::from)
        .map_err(|err| Error::Context(err.into()))?;
        store.put(&pointer_key, pointer_bytes).await?;
        Ok(())
    }

    #[tokio::test]
    async fn load_manifest_reads_pointer_and_manifest() -> Result<()> {
        let store = InMemoryStore::default();
        let ns = "demo";
        let view = sample_manifest(7);
        seed_manifest(&store, ns, &view).await?;

        let loaded = load_manifest(&store, ns).await?;

        assert_eq!(loaded.epoch, view.epoch);
        assert_eq!(loaded.namespace.dim, view.namespace.dim);
        assert_eq!(loaded.parts.len(), 1);
        assert_eq!(loaded.delete_parts.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn load_manifest_detects_etag_mismatch() {
        let store = InMemoryStore::default();
        let ns = "demo";
        let mut view = sample_manifest(3);
        seed_manifest(&store, ns, &view).await.unwrap();

        // Overwrite manifest with different content to desynchronise ETag values.
        view.parts[0].part_id = PartId("p2".to_string());
        let manifest_key = manifest_epoch_key(ns, 3);
        let _ = store
            .put(
                &manifest_key,
                Bytes::from(serde_json::to_vec(&view).unwrap()),
            )
            .await
            .unwrap();

        let err = load_manifest(&store, ns).await.unwrap_err();
        assert!(format!("{err}").contains("etag mismatch"));
    }

    #[tokio::test]
    async fn publish_manifest_writes_new_epoch_and_pointer() -> Result<()> {
        let store = InMemoryStore::default();
        let ns = "demo";
        let current = sample_manifest(5);
        seed_manifest(&store, ns, &current).await?;

        let next = sample_manifest(6);
        publish_manifest(&store, ns, &next).await?;

        let loaded = load_manifest(&store, ns).await?;
        assert_eq!(loaded.epoch, 6);
        assert_eq!(loaded.parts[0].part_id, PartId("p1".to_string()));
        Ok(())
    }

    #[tokio::test]
    async fn publish_manifest_rejects_stale_epoch() {
        let store = InMemoryStore::default();
        let ns = "demo";
        let current = sample_manifest(4);
        seed_manifest(&store, ns, &current).await.unwrap();

        let stale = sample_manifest(4);
        let err = publish_manifest(&store, ns, &stale).await.unwrap_err();
        assert!(format!("{err}").contains("refusing to publish stale"));
    }
}
