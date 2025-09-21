# Recall Drift Remediation Runbook

Recall drift occurs when the ANN path returns fewer true positives at a fixed `top_k` than the namespace SLO allows. This playbook outlines how to diagnose the regression and apply mitigations without permanently inflating query latency.

## Detection

- Automated checks of `/_debug/recall` fall below the configured target (for example, recall@10 < 0.95).【F:docs/design.md†L286-L308】
- Live A/B experiments or canary traffic show widening gaps between ANN and FP32 rerankers.
- Feature or schema changes (new filters, updated embeddings) precede a sudden recall drop.

## Immediate Stabilization

1. **Verify the measurement.** Run `POST /v1/namespaces/:ns/_debug/recall` with the same parameters the alert used and confirm the drop is reproducible.【F:docs/design.md†L286-L308】
2. **Enable FP32 rerank temporarily.** Override queries with `{ "ann_params": { "rerank_mode": "fp32" } }` for critical workloads while the investigation proceeds. Expect higher tail latency.【F:docs/design.md†L286-L330】
3. **Increase probes.** Bump `nprobe_ratio` (either per-query override or by raising the namespace `recall_budget`) to expand the candidate set searched before rerank.【F:docs/design.md†L286-L330】【F:docs/design.md†L358-L376】

## Root Cause Analysis

- **Embedding drift:** Confirm the upstream model change preserved cosine/Euclidean assumptions; mismatched normalization often degrades recall.
- **Indexer lag:** Large `wal_highwater - indexed_wal` gaps mean queries fall back to WAL tail scans, which can surface stale postings. Allow indexers to catch up before drawing conclusions.【F:crates/elax-store/src/lib.rs†L282-L333】
- **Cache churn:** If hot lists now evict due to working-set growth, pair this runbook with cache pinning to restore warm performance.【F:crates/elax-cache/src/lib.rs†L320-L359】

## Long-Term Mitigations

1. **Tune namespace defaults.** Update the namespace configuration so `recall_budget` yields the higher `nprobe_ratio` permanently once the latency impact is deemed acceptable.【F:docs/design.md†L358-L376】
2. **Adjust rerank scale.** Increase `rerank_scale` to feed more candidates into ERQ or FP32 rerankers when filters or BM25 blending reduce headroom.【F:docs/design.md†L286-L330】
3. **Rebuild codebooks.** If corpus drift is severe, schedule a re-training job for IVF centroids and ERQ codebooks; stale partitions reduce recall even with high probe counts.【F:docs/design.md†L120-L174】
4. **Expand hardware.** For chronic cache pressure or tight latency budgets, allocate more RAM/NVMe per query node to keep additional postings resident.【F:docs/design.md†L24-L80】

## Validation

- Re-run the recall endpoint after each change and log `avg_recall`, `avg_ann_count`, and `avg_exhaustive_count` deltas.【F:docs/design.md†L286-L308】
- Track P95/P99 latency to ensure mitigations do not violate SLOs; roll back individual steps if they overshoot targets.
- Once recall stabilizes for 24 hours, revert any temporary FP32 overrides if latency is a concern.

Record the final configuration (probe ratios, rerank mode, cache adjustments) and link to supporting dashboards in the incident tracker for historical reference.
