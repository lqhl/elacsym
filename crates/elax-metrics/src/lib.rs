//! Metrics exporters and instrumentation wiring for elacsym components.

use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use metrics::{describe_counter, describe_gauge, describe_histogram, Unit};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Initialize the global Prometheus recorder if it has not been installed yet.
///
/// The recorder is shared across crates and provides a [`PrometheusHandle`] that
/// can be used to gather metrics snapshots for tests and the `/metrics`
/// endpoint. Subsequent calls are no-ops.
pub fn init() -> Result<()> {
    if HANDLE.get().is_some() {
        return Ok(());
    }

    let builder = PrometheusBuilder::new();
    let handle = builder
        .install_recorder()
        .map_err(|err| anyhow!("installing metrics recorder: {err}"))?;
    register_static_metadata();
    HANDLE
        .set(handle)
        .map_err(|_| anyhow!("metrics recorder already initialized"))?;
    Ok(())
}

fn register_static_metadata() {
    describe_counter!(
        "elax_core_write_requests_total",
        Unit::Count,
        "Number of write batches applied per namespace"
    );
    describe_histogram!(
        "elax_core_write_latency_seconds",
        Unit::Seconds,
        "Latency distribution for write batches"
    );
    describe_counter!(
        "elax_core_query_requests_total",
        Unit::Count,
        "Number of query executions per namespace"
    );
    describe_histogram!(
        "elax_core_query_latency_seconds",
        Unit::Seconds,
        "Latency distribution for query execution"
    );
    describe_counter!(
        "elax_cache_hits_total",
        Unit::Count,
        "Cache hit count grouped by namespace and asset kind"
    );
    describe_counter!(
        "elax_cache_misses_total",
        Unit::Count,
        "Cache miss count grouped by namespace and asset kind"
    );
    describe_counter!(
        "elax_cache_evictions_total",
        Unit::Count,
        "Number of cache evictions"
    );
    describe_gauge!(
        "elax_cache_ram_bytes",
        Unit::Bytes,
        "Tracked RAM usage of the cache"
    );
    describe_gauge!(
        "elax_core_rows_cached",
        Unit::Count,
        "Number of rows currently resident in the namespace state cache"
    );
    describe_counter!(
        "elax_indexer_parts_total",
        Unit::Count,
        "Number of parts materialized by the indexer"
    );
    describe_counter!(
        "elax_indexer_compactions_total",
        Unit::Count,
        "Number of index compaction operations"
    );
    describe_histogram!(
        "elax_indexer_run_latency_seconds",
        Unit::Seconds,
        "Latency distribution for indexer runs"
    );
    describe_counter!(
        "elax_api_requests_total",
        Unit::Count,
        "HTTP API request counter grouped by route and status"
    );
}

/// Returns the Prometheus handle for metrics exposition if initialization has
/// completed.
pub fn handle() -> Option<&'static PrometheusHandle> {
    HANDLE.get()
}

/// Gather the current metrics snapshot as text encoded in Prometheus exposition
/// format.
pub fn gather() -> Result<String> {
    let handle = HANDLE
        .get()
        .ok_or_else(|| anyhow!("metrics recorder has not been initialized"))?;
    let rendered = handle.render();
    if rendered.trim().is_empty() {
        Ok("# HELP elax_metrics_up Exporter health indicator\n# TYPE elax_metrics_up gauge\nelax_metrics_up 1\n".to_string())
    } else {
        Ok(rendered)
    }
}
