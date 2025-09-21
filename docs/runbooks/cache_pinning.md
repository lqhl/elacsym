# Cache Pinning Runbook

Use this procedure to keep a namespace's search assets resident in the NVMe/RAM cache when eviction pressure would otherwise page them out. Pinning prevents hot IVF centroids, posting lists, and filter bitmaps from being reclaimed and is appropriate for premium tenants, seasonal surges, or latency regressions tied to cache churn.

## When to Pin

- `elax_cache_evictions_total` spikes for a namespace that should stay warm.
- P95/P99 latency rises in lockstep with cache misses or increased object-store GETs.
- Workload forecasting (product launch, marketing event) anticipates a short-term traffic burst exceeding cache capacity.

Before pinning, verify that the namespace's working set fits within the configured RAM budget; otherwise pinning will hold the entries on NVMe but still stream data from disk.

## Prerequisites

- Object-store credentials that allow updating `{org}/{namespace}/router.json`.
- The latest `elax-cli` or an equivalent admin tool capable of uploading router snapshots.
- Query nodes on a build that observes the `pin_hot` flag and calls `Cache::pin_namespace` during router refresh.

## Procedure

1. **Inspect current router state.** Download the namespace router to confirm existing metadata and whether another operator already pinned it.
   ```bash
   aws s3 cp s3://$ORG/$NS/router.json /tmp/router.json
   jq '.' /tmp/router.json
   ```
   The router document contains `"pin_hot": false` by default.【F:crates/elax-store/src/lib.rs†L282-L308】

2. **Set `pin_hot` to `true`.** Apply a JSON patch and push it back to object storage (or use `elax-cli namespaces pin --pin true`).
   ```bash
   jq '.pin_hot = true' /tmp/router.json > /tmp/router-pinned.json
   aws s3 cp /tmp/router-pinned.json s3://$ORG/$NS/router.json
   ```
   On the next router poll, query nodes will mark the namespace as pinned in the cache layer.【F:crates/elax-cache/src/lib.rs†L181-L207】

3. **Pre-warm the cache.** Issue `GET /v1/namespaces/:ns/hint_cache_warm` from each query node and stream the payload into `elax-cli cache warm` (or an equivalent script) to prefetch recommended assets before traffic hits.【F:docs/design.md†L286-L308】

4. **Monitor metrics.** Confirm that `elax_cache_evictions_total{namespace="$NS"}` stops incrementing and that RAM usage stabilizes. Latency SLOs should recover as warm hits resume.【F:crates/elax-cache/src/lib.rs†L320-L359】

## Rollback

1. Repeat step 1 to fetch the router snapshot.
2. Set `pin_hot` to `false` and upload the router.
3. Observe cache metrics to ensure the namespace can now be evicted; this is required when reallocating cache to a different tenant.

## Troubleshooting

- **Pinned namespace still evicts:** Ensure every query node runs a build that wires the router flag into `Cache::pin_namespace`; older binaries may ignore `pin_hot`. Rolling restart the fleet after uploading the router if necessary.【F:crates/elax-cache/src/lib.rs†L320-L359】
- **NVMe fills despite pinning:** Pinning protects RAM residency; NVMe capacity planning is separate. Verify `CacheConfig::nvme_root` sizing before pinning additional namespaces.【F:crates/elax-cache/src/lib.rs†L160-L209】
- **Router writes race:** Use conditional uploads (`--expected-size`/`If-Match`) when possible so concurrent operators do not stomp changes. If races occur, re-download the router and reapply both sets of edits.

Document any pin/unpin events in the on-call journal for future capacity reviews.
