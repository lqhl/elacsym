# Configuration

Elacsym is configured via a TOML configuration file (`config.toml`) or environment variables.

## Configuration File

Default location: `./config.toml` (current directory)

Override: Set `ELACSYM_CONFIG` environment variable:
```bash
ELACSYM_CONFIG=/etc/elacsym/config.toml ./elacsym
```

## Full Configuration Example

```toml
[server]
host = "0.0.0.0"
port = 3000

[storage]
# Storage backend: "s3" or "local"
backend = "local"

[storage.local]
root_path = "./data"

[storage.s3]
bucket = "elacsym-data"
region = "us-west-2"
# endpoint = "http://localhost:9000"  # For MinIO/Ceph

[cache]
memory_size = 4294967296  # 4GB (bytes)
disk_size = 107374182400  # 100GB (bytes)
disk_path = "./cache"

[index]
default_metric = "cosine"  # "cosine", "l2", or "dot"

[compaction]
enabled = true
interval_secs = 3600       # Check every 1 hour
max_segments = 100         # Trigger when segment count exceeds this
max_total_docs = 1000000   # Trigger when total docs exceed this

[logging]
level = "info"  # "trace", "debug", "info", "warn", "error"
format = "json" # "json" or "text"

[distributed]
# Optional: Enable distributed mode
enabled = false
node_id = "indexer-0"
role = "indexer"  # "indexer" or "query"

[distributed.indexer_cluster]
# List of all indexer nodes (for consistent hashing)
nodes = ["indexer-0", "indexer-1", "indexer-2"]
```

## Configuration Sections

### [server]

HTTP server configuration.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `host` | string | `"0.0.0.0"` | Bind address (use `"127.0.0.1"` for localhost only) |
| `port` | integer | `3000` | HTTP port |

**Environment Variables**:
- `ELACSYM_HOST`: Override `host`
- `ELACSYM_PORT`: Override `port`

**Example**:
```bash
ELACSYM_HOST=127.0.0.1 ELACSYM_PORT=8080 ./elacsym
```

---

### [storage]

Storage backend configuration.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `backend` | string | `"local"` | Storage type: `"local"` or `"s3"` |

**Environment Variables**:
- `ELACSYM_STORAGE_BACKEND`: Override `backend`

---

### [storage.local]

Local filesystem storage (for development and single-node deployments).

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `root_path` | string | `"./data"` | Root directory for all data |

**Environment Variables**:
- `ELACSYM_STORAGE_PATH`: Override `root_path`

**Example**:
```bash
ELACSYM_STORAGE_PATH=/var/lib/elacsym ./elacsym
```

**Directory Structure**:
```
./data/
  namespace1/
    manifest.json
    segments/
      seg_00001.parquet
    indexes/
      vector_index.bin
    wal/
      000001.wal
  namespace2/
    ...
```

**Permissions**:
- Elacsym needs read/write/execute permissions on `root_path`
- Recommended: Create dedicated user and set ownership

---

### [storage.s3]

S3-compatible object storage (for production and distributed deployments).

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `bucket` | string | Required | S3 bucket name |
| `region` | string | Required | AWS region (e.g., `"us-west-2"`) |
| `endpoint` | string | Optional | Custom endpoint for MinIO/Ceph (e.g., `"http://localhost:9000"`) |

**Environment Variables**:
- `ELACSYM_S3_BUCKET`: Override `bucket`
- `ELACSYM_S3_REGION`: Override `region`
- `ELACSYM_S3_ENDPOINT`: Override `endpoint`

**AWS Credentials**:

Elacsym uses the AWS SDK default credential chain:
1. Environment variables (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`)
2. AWS credentials file (`~/.aws/credentials`)
3. IAM instance profile (EC2/ECS)
4. EKS pod identity

**Example** (with credentials):
```bash
export AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE
export AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY
export ELACSYM_S3_BUCKET=my-elacsym-bucket
export ELACSYM_S3_REGION=us-west-2
./elacsym
```

**Example** (with MinIO):
```toml
[storage]
backend = "s3"

[storage.s3]
bucket = "elacsym"
region = "us-east-1"  # MinIO requires a region (any value works)
endpoint = "http://localhost:9000"
```

```bash
export AWS_ACCESS_KEY_ID=minioadmin
export AWS_SECRET_ACCESS_KEY=minioadmin
./elacsym
```

**Bucket Permissions** (IAM Policy):
```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": [
        "s3:GetObject",
        "s3:PutObject",
        "s3:DeleteObject",
        "s3:ListBucket"
      ],
      "Resource": [
        "arn:aws:s3:::elacsym-data",
        "arn:aws:s3:::elacsym-data/*"
      ]
    }
  ]
}
```

---

### [cache]

Hybrid cache configuration (memory + disk).

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `memory_size` | integer | `4294967296` (4GB) | Memory cache size in bytes |
| `disk_size` | integer | `107374182400` (100GB) | Disk cache size in bytes |
| `disk_path` | string | `"./cache"` | Directory for disk cache |

**Environment Variables**:
- `ELACSYM_CACHE_MEMORY_SIZE`: Override `memory_size` (in bytes)
- `ELACSYM_CACHE_DISK_SIZE`: Override `disk_size` (in bytes)
- `ELACSYM_CACHE_DISK_PATH`: Override `disk_path`

**Sizing Recommendations**:

| Workload | Memory Cache | Disk Cache | Notes |
|----------|--------------|------------|-------|
| Small (<1M vectors) | 1-2GB | 10-50GB | Most data fits in cache |
| Medium (1M-10M vectors) | 4-8GB | 100-500GB | Frequent queries cached |
| Large (>10M vectors) | 8-16GB | 500GB-2TB | Only hot data cached |

**Formula**:
- Memory cache: `num_namespaces * (manifest_size + index_size)`
  - Manifest: ~1MB per namespace
  - Vector index: ~0.1MB per 10,000 vectors (with RaBitQ compression)
  - Full-text index: ~5-10MB per 100,000 documents
- Disk cache: `hot_data_size * 1.5`
  - Hot data: Frequently queried segments
  - Rule of thumb: 10-20% of total data

**Example**:
```bash
# 8GB memory, 200GB disk
ELACSYM_CACHE_MEMORY_SIZE=8589934592 \
ELACSYM_CACHE_DISK_SIZE=214748364800 \
./elacsym
```

---

### [index]

Index configuration.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `default_metric` | string | `"cosine"` | Default distance metric: `"cosine"`, `"l2"`, or `"dot"` |

**Notes**:
- Can be overridden per namespace in schema
- Changing default does not affect existing namespaces

---

### [compaction]

Background compaction configuration.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enabled` | boolean | `true` | Enable background compaction |
| `interval_secs` | integer | `3600` (1 hour) | Check interval in seconds |
| `max_segments` | integer | `100` | Trigger compaction when segment count exceeds this |
| `max_total_docs` | integer | `1000000` | Trigger compaction when total documents exceed this |

**Environment Variables**:
- `ELACSYM_COMPACTION_ENABLED`: Override `enabled` (true/false)
- `ELACSYM_COMPACTION_INTERVAL_SECS`: Override `interval_secs`
- `ELACSYM_COMPACTION_MAX_SEGMENTS`: Override `max_segments`

**Compaction Behavior**:
- Runs in background thread (non-blocking)
- Merges up to 10 smallest segments per compaction
- Rebuilds vector and full-text indexes
- Atomically updates manifest

**Tuning**:
- **High write rate**: Lower `max_segments` (e.g., 50) to trigger more frequently
- **Low write rate**: Increase `max_segments` (e.g., 200) to reduce overhead
- **Testing**: Set `interval_secs = 10` for fast iteration

**Disable** (not recommended for production):
```toml
[compaction]
enabled = false
```

---

### [logging]

Logging configuration.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `level` | string | `"info"` | Log level: `"trace"`, `"debug"`, `"info"`, `"warn"`, `"error"` |
| `format` | string | `"json"` | Log format: `"json"` or `"text"` |

**Environment Variables**:
- `RUST_LOG`: Override `level` (more fine-grained control)

**RUST_LOG Examples**:
```bash
# All logs at info level
RUST_LOG=info ./elacsym

# Elacsym at debug, everything else at warn
RUST_LOG=elacsym=debug,warn ./elacsym

# Trace specific modules
RUST_LOG=elacsym::namespace=trace,elacsym::index=debug ./elacsym

# Include dependencies
RUST_LOG=elacsym=debug,tower_http=debug,aws_sdk_s3=debug ./elacsym
```

**Log Format**:
- **JSON** (recommended for production): Structured, machine-readable
  ```json
  {"timestamp":"2025-10-06T10:30:45Z","level":"INFO","message":"Server started","port":3000}
  ```
- **Text** (recommended for development): Human-readable
  ```
  2025-10-06T10:30:45Z INFO elacsym: Server started port=3000
  ```

---

### [distributed]

Distributed deployment configuration (optional).

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enabled` | boolean | `false` | Enable distributed mode |
| `node_id` | string | Required | Unique node identifier (e.g., `"indexer-0"`) |
| `role` | string | Required | Node role: `"indexer"` or `"query"` |

**Environment Variables**:
- `ELACSYM_NODE_ID`: Override `node_id`
- `ELACSYM_NODE_ROLE`: Override `role`

---

### [distributed.indexer_cluster]

Indexer cluster configuration (only for indexer nodes).

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `nodes` | array | Required | List of all indexer node IDs |

**Example**:
```toml
[distributed]
enabled = true
node_id = "indexer-0"
role = "indexer"

[distributed.indexer_cluster]
nodes = ["indexer-0", "indexer-1", "indexer-2"]
```

**Namespace Sharding**:
- Uses consistent hashing to assign namespaces to indexers
- Each namespace handled by exactly one indexer
- Query nodes can read from any indexer's namespaces

**See**: `docs/deployment.md` for multi-node setup guide.

---

## Environment Variables Summary

All configuration options can be overridden via environment variables:

| Config Path | Environment Variable | Example |
|-------------|---------------------|---------|
| `server.host` | `ELACSYM_HOST` | `0.0.0.0` |
| `server.port` | `ELACSYM_PORT` | `3000` |
| `storage.backend` | `ELACSYM_STORAGE_BACKEND` | `local` or `s3` |
| `storage.local.root_path` | `ELACSYM_STORAGE_PATH` | `/var/lib/elacsym` |
| `storage.s3.bucket` | `ELACSYM_S3_BUCKET` | `my-bucket` |
| `storage.s3.region` | `ELACSYM_S3_REGION` | `us-west-2` |
| `storage.s3.endpoint` | `ELACSYM_S3_ENDPOINT` | `http://localhost:9000` |
| `cache.memory_size` | `ELACSYM_CACHE_MEMORY_SIZE` | `4294967296` |
| `cache.disk_size` | `ELACSYM_CACHE_DISK_SIZE` | `107374182400` |
| `cache.disk_path` | `ELACSYM_CACHE_DISK_PATH` | `./cache` |
| `compaction.enabled` | `ELACSYM_COMPACTION_ENABLED` | `true` or `false` |
| `compaction.interval_secs` | `ELACSYM_COMPACTION_INTERVAL_SECS` | `3600` |
| `compaction.max_segments` | `ELACSYM_COMPACTION_MAX_SEGMENTS` | `100` |
| `distributed.node_id` | `ELACSYM_NODE_ID` | `indexer-0` |
| `distributed.role` | `ELACSYM_NODE_ROLE` | `indexer` or `query` |
| Logging | `RUST_LOG` | `info` or `elacsym=debug` |
| AWS credentials | `AWS_ACCESS_KEY_ID` | `AKIAIOSFODNN7...` |
| AWS credentials | `AWS_SECRET_ACCESS_KEY` | `wJalrXUtnFEMI...` |

**Precedence**: Environment variables > Config file > Defaults

---

## Quick Start Configurations

### Development (Local Storage)

```toml
[server]
host = "127.0.0.1"
port = 3000

[storage]
backend = "local"

[storage.local]
root_path = "./data"

[cache]
memory_size = 1073741824   # 1GB
disk_size = 10737418240    # 10GB
disk_path = "./cache"

[logging]
level = "debug"
format = "text"
```

### Production (Single Node, S3)

```toml
[server]
host = "0.0.0.0"
port = 3000

[storage]
backend = "s3"

[storage.s3]
bucket = "elacsym-production"
region = "us-west-2"

[cache]
memory_size = 8589934592   # 8GB
disk_size = 214748364800   # 200GB
disk_path = "/var/cache/elacsym"

[compaction]
enabled = true
interval_secs = 3600
max_segments = 100

[logging]
level = "info"
format = "json"
```

### Production (Distributed, 3 Indexers)

**Indexer 0**:
```toml
[server]
host = "0.0.0.0"
port = 3000

[storage]
backend = "s3"

[storage.s3]
bucket = "elacsym-production"
region = "us-west-2"

[cache]
memory_size = 8589934592
disk_size = 214748364800
disk_path = "/var/cache/elacsym"

[distributed]
enabled = true
node_id = "indexer-0"
role = "indexer"

[distributed.indexer_cluster]
nodes = ["indexer-0", "indexer-1", "indexer-2"]

[logging]
level = "info"
format = "json"
```

**Indexer 1, 2**: Same config, change `node_id` to `"indexer-1"` and `"indexer-2"`.

**Query Node**:
```toml
[distributed]
enabled = true
node_id = "query-0"
role = "query"

[distributed.indexer_cluster]
nodes = ["indexer-0", "indexer-1", "indexer-2"]

# ... rest same as indexers ...
```

---

## Configuration Validation

Elacsym validates configuration on startup and fails fast with clear error messages:

**Example Errors**:
```
ERROR: Invalid storage backend: "unknown" (expected "local" or "s3")
ERROR: S3 bucket name is required when storage.backend = "s3"
ERROR: cache.memory_size must be at least 1MB (1048576 bytes)
ERROR: compaction.max_segments must be greater than 0
```

**Check Configuration**:
```bash
# Dry-run: validate config without starting server
./elacsym --check-config
```

---

## Performance Tuning

See `docs/performance.md` for detailed tuning guide. Quick tips:

1. **Memory cache**: Size to fit all indexes
   - Formula: `num_namespaces * (1MB + 0.01MB * vectors/1000)`
2. **Disk cache**: Size to fit 10-20% of total data
3. **Compaction interval**: Lower for high write rate, higher for low write rate
4. **Compaction thresholds**: Lower to keep segment count small (better query performance)

---

## Security Notes

⚠️ **Elacsym v0.1.0 has no built-in authentication**. Deploy behind a reverse proxy with auth (nginx, Envoy, etc.) for production.

**Recommended Setup**:
```
Client → nginx (with API key auth) → Elacsym
```

**nginx Example**:
```nginx
location /v1/ {
    if ($http_x_api_key != "your-secret-key") {
        return 401;
    }
    proxy_pass http://localhost:3000;
}
```

---

## Troubleshooting

### Server won't start

**Check**:
1. Port already in use: `lsof -i :3000`
2. Storage path permissions: `ls -la ./data`
3. S3 credentials: `aws s3 ls s3://your-bucket`
4. Config file syntax: `./elacsym --check-config`

### High memory usage

**Solutions**:
1. Reduce `cache.memory_size`
2. Increase `compaction.max_segments` (fewer compactions = less temp memory)
3. Monitor with: `docker stats` or `htop`

### Slow queries

**Solutions**:
1. Increase `cache.disk_size` (more segments cached)
2. Use faster disk for cache (SSD/NVMe)
3. Check cache hit rate: Look for `cache miss` in logs

### Compaction too frequent

**Solutions**:
1. Increase `compaction.max_segments` (e.g., 200)
2. Increase `compaction.interval_secs` (e.g., 7200)
3. Disable temporarily: `compaction.enabled = false`

### WAL files growing

**Check**:
- WAL rotation enabled (automatic)
- Check for errors in logs (failed writes prevent truncation)
- Manual cleanup (safe): `rm data/*/wal/*.wal` (only when server stopped)

---

## See Also

- [Architecture](architecture.md) - System design and components
- [Deployment](deployment.md) - Production deployment guide
- [Performance](performance.md) - Performance tuning guide
- [API Reference](api-reference.md) - HTTP API documentation
