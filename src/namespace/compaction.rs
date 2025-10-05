//! Background compaction manager
//!
//! This module provides automatic background compaction for namespaces.
//! It periodically checks all namespaces and triggers compaction when needed.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time;

use crate::namespace::Namespace;
use crate::Result;

/// Configuration for compaction behavior
#[derive(Debug, Clone)]
pub struct CompactionConfig {
    /// Check interval in seconds (default: 3600 = 1 hour)
    pub interval_secs: u64,

    /// Maximum number of segments before compaction is triggered (default: 100)
    pub max_segments: usize,

    /// Maximum total documents before compaction is triggered (default: 1M)
    pub max_total_docs: usize,

    /// Minimum number of segments to merge (default: 2)
    pub min_segments_to_merge: usize,

    /// Maximum number of segments to merge at once (default: 10)
    pub max_segments_to_merge: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            interval_secs: 3600, // 1 hour
            max_segments: 100,
            max_total_docs: 1_000_000,
            min_segments_to_merge: 2,
            max_segments_to_merge: 10,
        }
    }
}

impl CompactionConfig {
    /// Create a new CompactionConfig with custom values
    pub fn new(interval_secs: u64, max_segments: usize, max_total_docs: usize) -> Self {
        Self {
            interval_secs,
            max_segments,
            max_total_docs,
            ..Default::default()
        }
    }

    /// Create a config for testing (faster intervals)
    #[cfg(test)]
    pub fn for_testing() -> Self {
        Self {
            interval_secs: 1, // 1 second for tests
            max_segments: 5,
            max_total_docs: 100,
            min_segments_to_merge: 2,
            max_segments_to_merge: 10,
        }
    }
}

/// Background compaction manager
///
/// Runs a background task that periodically checks namespaces
/// and triggers compaction when needed.
pub struct CompactionManager {
    config: CompactionConfig,
    running: Arc<RwLock<bool>>,
}

impl CompactionManager {
    /// Create a new CompactionManager
    pub fn new(config: CompactionConfig) -> Self {
        Self {
            config,
            running: Arc::new(RwLock::new(false)),
        }
    }

    /// Create a disabled CompactionManager
    pub fn disabled() -> Self {
        Self {
            config: CompactionConfig {
                interval_secs: 0,
                max_segments: usize::MAX,
                max_total_docs: usize::MAX,
                min_segments_to_merge: 0,
                max_segments_to_merge: 0,
            },
            running: Arc::new(RwLock::new(false)),
        }
    }

    /// Check if the manager is running
    pub async fn is_running(&self) -> bool {
        *self.running.read().await
    }

    /// Check if compaction is enabled
    pub fn is_enabled(&self) -> bool {
        self.config.interval_secs > 0
    }

    /// Start the compaction background task for a single namespace
    ///
    /// This spawns a background tokio task that runs until stop() is called.
    pub async fn start_for_namespace(&self, namespace: Arc<Namespace>) -> Result<()> {
        if !self.is_enabled() {
            tracing::info!("Compaction is disabled");
            return Ok(());
        }

        // Check if already running
        {
            let mut running = self.running.write().await;
            if *running {
                tracing::warn!("CompactionManager already running for this namespace");
                return Ok(());
            }
            *running = true;
        }

        let config = self.config.clone();
        let running = self.running.clone();

        // Spawn background task
        tokio::spawn(async move {
            tracing::info!(
                "Starting compaction manager with interval: {} seconds",
                config.interval_secs
            );

            let mut interval = time::interval(Duration::from_secs(config.interval_secs));
            interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

            loop {
                // Wait for next tick
                interval.tick().await;

                // Check if we should stop
                {
                    let running_guard = running.read().await;
                    if !*running_guard {
                        tracing::info!("CompactionManager stopped");
                        break;
                    }
                }

                // Check if compaction is needed
                match Self::check_and_compact(&namespace, &config).await {
                    Ok(compacted) => {
                        if compacted {
                            tracing::info!("Compaction completed successfully");
                        }
                    }
                    Err(e) => {
                        tracing::error!("Compaction failed: {}", e);
                        // Don't stop the loop - keep trying on next interval
                    }
                }
            }
        });

        Ok(())
    }

    /// Stop the background compaction task
    pub async fn stop(&self) {
        let mut running = self.running.write().await;
        *running = false;
        tracing::info!("Stopping compaction manager");
    }

    /// Check if compaction is needed and execute it
    ///
    /// Returns Ok(true) if compaction was performed, Ok(false) if not needed
    async fn check_and_compact(
        namespace: &Arc<Namespace>,
        config: &CompactionConfig,
    ) -> Result<bool> {
        // Check if compaction is needed
        let should_compact = namespace.should_compact_with_config(config).await;

        if !should_compact {
            tracing::debug!("Compaction not needed");
            return Ok(false);
        }

        let segment_count = namespace.segment_count().await;
        tracing::info!("Compaction needed - segment_count: {}", segment_count);

        // Perform compaction
        namespace.compact().await?;

        Ok(true)
    }
}

impl Drop for CompactionManager {
    fn drop(&mut self) {
        // Note: We can't await in Drop, so we just set the flag
        // The background task will stop on the next interval check
        if let Ok(mut running) = self.running.try_write() {
            *running = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::local::LocalStorage;
    use crate::types::{
        AttributeSchema, AttributeType, AttributeValue, DistanceMetric, Document, FullTextConfig,
        Schema,
    };
    use std::collections::HashMap;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_compaction_config_default() {
        let config = CompactionConfig::default();
        assert_eq!(config.interval_secs, 3600);
        assert_eq!(config.max_segments, 100);
        assert_eq!(config.max_total_docs, 1_000_000);
    }

    #[tokio::test]
    async fn test_compaction_manager_lifecycle() {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());

        // Create a simple namespace
        let schema = Schema {
            vector_dim: 64,
            vector_metric: DistanceMetric::L2,
            attributes: HashMap::new(),
        };

        let namespace = Arc::new(
            Namespace::create("test_ns".to_string(), schema, storage, None)
                .await
                .unwrap(),
        );

        let config = CompactionConfig::for_testing();
        let manager = CompactionManager::new(config);

        assert!(!manager.is_running().await);

        manager.start_for_namespace(namespace).await.unwrap();
        assert!(manager.is_running().await);

        manager.stop().await;

        // Give it a moment to process the stop signal
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn test_compaction_manager_triggers_compaction() {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());

        // Create namespace with simple schema
        let mut attributes = HashMap::new();
        attributes.insert(
            "title".to_string(),
            AttributeSchema {
                attr_type: AttributeType::String,
                indexed: false,
                full_text: FullTextConfig::Simple(false),
            },
        );

        let schema = Schema {
            vector_dim: 64,
            vector_metric: DistanceMetric::L2,
            attributes,
        };

        let namespace = Arc::new(
            Namespace::create("test_ns".to_string(), schema, storage, None)
                .await
                .unwrap(),
        );

        // Insert multiple small batches to create many segments
        for i in 0..6 {
            let mut attrs = HashMap::new();
            attrs.insert(
                "title".to_string(),
                AttributeValue::String(format!("Doc {}", i)),
            );

            let doc = Document {
                id: i,
                vector: Some(vec![i as f32; 64]),
                attributes: attrs,
            };

            namespace.upsert(vec![doc]).await.unwrap();
        }

        // Should have 6 segments now
        assert_eq!(namespace.segment_count().await, 6);

        // Start compaction manager with config that triggers on 5 segments
        let config = CompactionConfig {
            interval_secs: 1,
            max_segments: 5, // Trigger compaction
            max_total_docs: 1000,
            min_segments_to_merge: 2,
            max_segments_to_merge: 10,
        };

        let manager = CompactionManager::new(config);
        manager
            .start_for_namespace(namespace.clone())
            .await
            .unwrap();

        // Wait for compaction to happen (should trigger within 1-2 seconds)
        tokio::time::sleep(Duration::from_secs(3)).await;

        // Segment count should be reduced after compaction
        let final_count = namespace.segment_count().await;
        assert!(
            final_count < 6,
            "Expected segment count < 6, got {}",
            final_count
        );

        manager.stop().await;
    }

    #[tokio::test]
    async fn test_check_and_compact() {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());

        let schema = Schema {
            vector_dim: 64,
            vector_metric: DistanceMetric::L2,
            attributes: HashMap::new(),
        };

        let namespace = Arc::new(
            Namespace::create("test_ns".to_string(), schema, storage, None)
                .await
                .unwrap(),
        );

        let config = CompactionConfig::for_testing();

        // No segments yet, should not compact
        let result = CompactionManager::check_and_compact(&namespace, &config).await;
        assert!(result.is_ok());
        assert!(!result.unwrap()); // false = no compaction

        // Add some segments
        for i in 0..3 {
            let doc = Document {
                id: i,
                vector: Some(vec![i as f32; 64]),
                attributes: HashMap::new(),
            };
            namespace.upsert(vec![doc]).await.unwrap();
        }

        // Now should not compact (only 3 segments, threshold is 5)
        let result = CompactionManager::check_and_compact(&namespace, &config).await;
        assert!(result.is_ok());
        assert!(!result.unwrap());

        // Add more segments to exceed threshold
        for i in 3..6 {
            let doc = Document {
                id: i,
                vector: Some(vec![i as f32; 64]),
                attributes: HashMap::new(),
            };
            namespace.upsert(vec![doc]).await.unwrap();
        }

        // Now should compact
        let result = CompactionManager::check_and_compact(&namespace, &config).await;
        assert!(result.is_ok());
        assert!(result.unwrap()); // true = compaction performed
    }
}
