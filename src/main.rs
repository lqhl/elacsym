//! Elacsym server binary

use std::sync::Arc;

use anyhow::{anyhow, bail, Context};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use elacsym::api::{create_router, AppState, NodeRole};
use elacsym::cache::{CacheConfig, CacheManager};
use elacsym::config::{AppConfig, DistributedRole, LogFormat};
use elacsym::namespace::{CompactionConfig, NamespaceManager};
use elacsym::sharding::{IndexerCluster, NodeConfig};
use elacsym::storage::create_storage;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = AppConfig::load().context("failed to load configuration")?;

    init_tracing(&config)?;

    // Resolve storage and WAL configuration
    let (storage_config, wal_config) = config
        .storage_runtime()
        .context("invalid storage configuration")?;

    let storage_backend = create_storage(storage_config).await?;
    let storage: Arc<dyn elacsym::storage::StorageBackend> = Arc::from(storage_backend);

    // Build cache if enabled
    let cache = build_cache(&config).await?;

    // Build compaction configuration
    let compaction_config = build_compaction_config(&config.compaction);

    // Determine node identity
    let node_id = resolve_node_id(&config);
    tracing::info!(node_id = %node_id, "Starting Elacsym node");

    // Instantiate namespace manager
    let manager = Arc::new(NamespaceManager::with_compaction_config(
        storage.clone(),
        cache.clone(),
        compaction_config.clone(),
        wal_config.clone(),
        node_id.clone(),
    ));

    // Determine distributed state
    let (state, role_description) = build_app_state(&config, manager.clone(), node_id.clone())?;
    tracing::info!(role = role_description, "Node role initialised");

    let router = create_router(state);

    // Start server
    let addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("failed to bind to {}", addr))?;
    tracing::info!(addr = %addr, "Listening for HTTP traffic");

    axum::serve(listener, router).await?;

    Ok(())
}

fn resolve_node_id(config: &AppConfig) -> String {
    config
        .distributed
        .as_ref()
        .and_then(|d| d.node_id.clone())
        .or_else(|| std::env::var("ELACSYM_NODE_ID").ok())
        .or_else(|| hostname::get().ok().and_then(|h| h.into_string().ok()))
        .unwrap_or_else(|| "elacsym-node".to_string())
}

async fn build_cache(config: &AppConfig) -> anyhow::Result<Option<Arc<CacheManager>>> {
    if std::env::var("ELACSYM_DISABLE_CACHE").is_ok() {
        tracing::info!("Cache disabled via environment variable");
        return Ok(None);
    }

    if config.cache.memory_size == 0 {
        tracing::info!("Cache disabled via configuration (memory_size = 0)");
        return Ok(None);
    }

    let cache_config = CacheConfig {
        memory_size: config.cache.memory_size,
        disk_size: config.cache.disk_size,
        disk_path: config.cache.disk_path.clone(),
    };

    tracing::info!(
        memory_mb = cache_config.memory_size / (1024 * 1024),
        disk_mb = cache_config.disk_size / (1024 * 1024),
        path = %cache_config.disk_path,
        "Initialising cache",
    );

    match CacheManager::new(cache_config).await {
        Ok(cache) => Ok(Some(Arc::new(cache))),
        Err(err) => {
            if std::env::var("ELACSYM_REQUIRE_CACHE").is_ok() {
                return Err(err.into());
            }
            tracing::warn!(error = %err, "Failed to initialise cache; continuing without it");
            Ok(None)
        }
    }
}

fn build_compaction_config(config: &elacsym::config::CompactionSection) -> CompactionConfig {
    if !config.enabled {
        return CompactionConfig {
            interval_secs: 0,
            max_segments: usize::MAX,
            max_total_docs: usize::MAX,
            ..CompactionConfig::default()
        };
    }

    CompactionConfig {
        interval_secs: config.interval_secs,
        max_segments: config.max_segments,
        max_total_docs: config.max_total_docs,
        ..CompactionConfig::default()
    }
}

fn init_tracing(config: &AppConfig) -> anyhow::Result<()> {
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(config.logging.level.clone()))
        .unwrap_or_else(|_| EnvFilter::new("elacsym=info"));

    let registry = tracing_subscriber::registry().with(env_filter);

    match config.logging.format {
        LogFormat::Json => {
            registry
                .with(tracing_subscriber::fmt::layer().json())
                .init();
        }
        LogFormat::Text => {
            registry.with(tracing_subscriber::fmt::layer()).init();
        }
    }

    Ok(())
}

fn build_app_state(
    config: &AppConfig,
    manager: Arc<NamespaceManager>,
    node_id: String,
) -> anyhow::Result<(AppState, &'static str)> {
    if let Some(distributed) = &config.distributed {
        if distributed.enabled {
            let (indexer_nodes, dist_role) = validate_distributed_config(distributed)?;

            let env_role_override = std::env::var("ELACSYM_NODE_ROLE")
                .ok()
                .map(|value| value.parse::<DistributedRole>())
                .transpose()
                .context("invalid ELACSYM_NODE_ROLE")?;

            let runtime_role = env_role_override
                .clone()
                .unwrap_or_else(|| dist_role.clone());

            let (cluster, role, description) = match runtime_role {
                DistributedRole::Indexer => {
                    let idx = indexer_nodes
                        .iter()
                        .position(|n| n == &node_id)
                        .ok_or_else(|| {
                            anyhow!(
                                "node_id '{}' not found in distributed.indexer_cluster.nodes",
                                node_id
                            )
                        })?;

                    if dist_role != DistributedRole::Indexer {
                        bail!(
                            "Role mismatch: node configured as {:?} but processing as Indexer. \
                             Check distributed.role in config",
                            dist_role
                        );
                    }

                    if let Some(env_role) = env_role_override.as_ref() {
                        if env_role != &DistributedRole::Indexer {
                            bail!(
                                "Role mismatch: node configured as {:?} but environment override set to {:?}",
                                dist_role,
                                env_role
                            );
                        }
                    }

                    let cluster = Arc::new(IndexerCluster::new(
                        NodeConfig::new(node_id.clone(), indexer_nodes.len(), idx),
                        indexer_nodes.clone(),
                    ));
                    (cluster, NodeRole::Indexer, "indexer")
                }
                DistributedRole::Query => {
                    if dist_role != DistributedRole::Query {
                        bail!(
                            "Role mismatch: node configured as {:?} but processing as Query",
                            dist_role
                        );
                    }

                    if let Some(env_role) = env_role_override.as_ref() {
                        if env_role != &DistributedRole::Query {
                            bail!(
                                "Role mismatch: node configured as {:?} but environment override set to {:?}",
                                dist_role,
                                env_role
                            );
                        }
                    }

                    let cluster = Arc::new(IndexerCluster::for_query(
                        node_id.clone(),
                        indexer_nodes.clone(),
                    ));
                    (cluster, NodeRole::Query, "query")
                }
            };

            manager.set_compaction_enabled(matches!(role, NodeRole::Indexer));

            let state = AppState::multi_node(manager, cluster, role);
            return Ok((state, description));
        }
    }

    manager.set_compaction_enabled(true);

    Ok((AppState::single_node(manager), "single-node"))
}

fn validate_distributed_config(
    distributed: &elacsym::config::DistributedSection,
) -> anyhow::Result<(Vec<String>, DistributedRole)> {
    let indexer = distributed.indexer.as_ref().context(
        "distributed.indexer_cluster.nodes must be specified when distributed mode is enabled",
    )?;

    if indexer.nodes.is_empty() {
        bail!("distributed.indexer_cluster.nodes must contain at least one indexer");
    }

    let role = distributed.role.clone().ok_or_else(|| {
        anyhow!("distributed.role must be specified when distributed mode is enabled")
    })?;

    Ok((indexer.nodes.clone(), role))
}

#[cfg(test)]
mod tests {
    use super::*;
    use elacsym::config::{DistributedSection, IndexerClusterSection};
    use elacsym::namespace::WalConfig;
    use elacsym::storage::local::LocalStorage;
    use std::sync::Arc;
    use tempfile::TempDir;

    struct EnvVarGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(ref value) = self.original {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn validate_distributed_config_errors_on_missing_nodes() {
        let distributed = DistributedSection {
            enabled: true,
            indexer: None,
            ..Default::default()
        };

        let err = validate_distributed_config(&distributed).unwrap_err();
        assert!(
            err.to_string()
                .contains("distributed.indexer_cluster.nodes must be specified"),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn validate_distributed_config_errors_on_empty_nodes() {
        let distributed = DistributedSection {
            enabled: true,
            indexer: Some(IndexerClusterSection { nodes: Vec::new() }),
            ..Default::default()
        };

        let err = validate_distributed_config(&distributed).unwrap_err();
        assert!(
            err.to_string()
                .contains("distributed.indexer_cluster.nodes must contain at least one indexer"),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn validate_distributed_config_requires_role() {
        let distributed = DistributedSection {
            enabled: true,
            indexer: Some(IndexerClusterSection {
                nodes: vec!["node-a".to_string()],
            }),
            ..Default::default()
        };

        let err = validate_distributed_config(&distributed).unwrap_err();
        assert!(
            err.to_string()
                .contains("distributed.role must be specified"),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn validate_distributed_config_returns_values() {
        let distributed = DistributedSection {
            enabled: true,
            role: Some(DistributedRole::Query),
            indexer: Some(IndexerClusterSection {
                nodes: vec!["node-a".to_string(), "node-b".to_string()],
            }),
            ..Default::default()
        };

        let (nodes, role) = validate_distributed_config(&distributed).expect("should succeed");
        assert_eq!(nodes, vec!["node-a", "node-b"]);
        assert_eq!(role, DistributedRole::Query);
    }

    #[test]
    fn test_role_mismatch_detection() {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());
        let wal_config = WalConfig::local(temp_dir.path().join("wal"));
        let manager = Arc::new(NamespaceManager::with_compaction_config(
            storage,
            None,
            CompactionConfig::default(),
            wal_config,
            "indexer-1".to_string(),
        ));

        let mut config = AppConfig::default();
        config.distributed = Some(DistributedSection {
            enabled: true,
            node_id: Some("indexer-1".to_string()),
            role: Some(DistributedRole::Indexer),
            indexer: Some(IndexerClusterSection {
                nodes: vec!["indexer-1".to_string()],
            }),
        });

        let _guard = EnvVarGuard::set("ELACSYM_NODE_ROLE", "query");

        let result = build_app_state(&config, manager, "indexer-1".to_string());
        let err = match result {
            Ok(_) => panic!("expected mismatch error"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("Role mismatch"),
            "unexpected error: {err:?}"
        );
    }
}
