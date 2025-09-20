//! Metrics exporters and instrumentation wiring.

use anyhow::Result;

/// Placeholder initialization that registers a no-op recorder.
pub fn init() -> Result<()> {
    metrics::register_counter!("elax_placeholder");
    Ok(())
}
