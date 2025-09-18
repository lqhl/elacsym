#![allow(dead_code)]

//! Candidate generation over IVF + RaBitQ coded data.

use common::{Error, NamespaceConfig, Result};
use tracing::instrument;

/// Planner output consumed by the search stage.
#[derive(Debug, Clone, Default)]
pub struct PartSearchPlan {
    pub k: usize,
    pub nprobe: usize,
    pub fallback: bool,
}

/// Compute the number of probes for a part using the documented heuristics.
pub fn plan_for_part(
    cfg: &NamespaceConfig,
    k_trained: usize,
    small_part_fallback: bool,
    probe_fraction: f32,
) -> PartSearchPlan {
    if small_part_fallback {
        return PartSearchPlan {
            k: 1,
            nprobe: 1,
            fallback: true,
        };
    }

    let k = k_trained.max(1);
    let raw = (probe_fraction * k as f32).round() as usize;
    let nprobe = raw.clamp(1, k.min(cfg.nprobe_cap.max(1)));
    PartSearchPlan {
        k,
        nprobe,
        fallback: false,
    }
}

/// Placeholder search entrypoint that will eventually perform IVF probing and candidate selection.
#[instrument]
pub async fn search_namespace(ns: &str) -> Result<()> {
    let _ = ns;
    Err(Error::Message(
        "stage-1 search has not been implemented yet".to_string(),
    ))
}
