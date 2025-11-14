//! Builder worker

use super::service::{BuilderConfig, BuilderService};
use elacsym_metadata::MetadataStore;
use elacsym_storage::BlobStorage;
use std::sync::Arc;
use tracing::info;

/// Builder worker that runs in background
pub struct BuilderWorker<S: MetadataStore> {
    service: Arc<BuilderService<S>>,
}

impl<S: MetadataStore + 'static> BuilderWorker<S> {
    pub fn new(service: Arc<BuilderService<S>>) -> Self {
        Self { service }
    }

    /// Start the worker
    pub async fn start(self) {
        info!("Starting builder worker");

        // Run periodic flush in background
        let service = Arc::clone(&self.service);
        tokio::spawn(async move {
            service.run_periodic_flush().await;
        });

        info!("Builder worker started");
    }
}
