# Agent Task Plan
## Steps
- [x] Analyse compaction initialization to confirm query nodes start background work.
- [x] Implement role-aware compaction gating in `NamespaceManager` so read-only nodes skip background tasks.
- [x] Update node setup code and helpers to disable compaction on query nodes.
- [x] Add regression tests ensuring query nodes avoid spawning compaction managers.
- [x] Run linting and test suites to verify changes.
## Progress
