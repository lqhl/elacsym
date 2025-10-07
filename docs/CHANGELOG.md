# Changelog

## [Unreleased] - v0.3.0

### Breaking Changes

#### WAL Configuration Now Mandatory

**What Changed**:
- WAL configuration is no longer optional
- All namespace operations now require a `WalConfig`
- `NamespaceManager` constructors now require `WalConfig` parameter

**Migration Guide**:

**Before (v0.2.0)**:
```rust
let manager = NamespaceManager::new(storage, node_id);
```

**After (v0.3.0)**:
```rust
use elacsym::namespace::WalConfig;

// For local filesystem:
let wal_config = WalConfig::local("./wal");
let manager = NamespaceManager::new(storage, wal_config, node_id);

// For S3:
let wal_config = WalConfig::s3(Some("tenant-prefix".to_string()));
let manager = NamespaceManager::new(storage, wal_config, node_id);
```

**Rationale**: Making WAL mandatory ensures data durability in all deployment scenarios, especially critical for distributed setups.

### New Features
- Distributed deployment with indexer/query node roles
- Centralized configuration system via TOML + environment variables
- S3-compatible storage backend (MinIO support)
- Role-aware compaction gating

### Bug Fixes
- Fixed query nodes incorrectly starting compaction managers
