# Deployment Guide

## Table of Contents
1. [Single-Node Deployment](#single-node-deployment)
2. [Distributed Deployment](#distributed-deployment)
   - [Architecture Overview](#architecture)
   - [MinIO Setup](#minio-setup)
   - [Multi-Node Configuration](#multi-node-configuration)
   - [Deployment Examples](#deployment-examples)
3. [Production Best Practices](#production-best-practices)
4. [Troubleshooting](#troubleshooting)
5. [Performance Tuning](#performance-tuning)

## Single-Node Deployment

Single-node mode is ideal for local development, proofs of concept, and small datasets (<1M documents).

### Local Storage Quick Start

```bash
# Build the release binary
cargo build --release

# Prepare data + cache directories
mkdir -p /var/lib/elacsym /var/cache/elacsym

# Launch the server with local filesystem storage
ELACSYM_STORAGE_BACKEND=local \
ELACSYM_STORAGE_LOCAL_ROOT_PATH=/var/lib/elacsym \
ELACSYM_CACHE_DISK_PATH=/var/cache/elacsym \
./target/release/elacsym
```

### S3 Storage Quick Start

```bash
# Provide S3 credentials
export AWS_ACCESS_KEY_ID=your_access_key
export AWS_SECRET_ACCESS_KEY=your_secret_key

# Launch the server backed by S3
ELACSYM_STORAGE_BACKEND=s3 \
ELACSYM_STORAGE_S3_BUCKET=elacsym-production \
ELACSYM_STORAGE_S3_REGION=us-west-2 \
./target/release/elacsym
```

## Distributed Deployment

Distributed mode unlocks horizontal scalability with dedicated indexer and query nodes backed by a shared S3-compatible object store.

### Architecture

#### Node Roles
- **Indexer Nodes**: Own namespace shards, handle writes (upsert), maintain manifests, and run compaction.
- **Query Nodes**: Stateless routers that forward reads to the responsible indexers. They never run compaction or accept writes.

#### Sharding Strategy
Namespaces are assigned to indexers using consistent hashing:
- Hash function: `seahash(namespace) % num_indexers`
- Provides even distribution across indexers
- Query routing is deterministic; no data migration on reads

### MinIO Setup

#### Docker Compose (Development)
```yaml
version: '3.8'

services:
  minio:
    image: minio/minio:latest
    command: server /data --console-address ":9001"
    ports:
      - "9000:9000"
      - "9001:9001"
    environment:
      MINIO_ROOT_USER: minioadmin
      MINIO_ROOT_PASSWORD: minioadmin
    volumes:
      - minio_data:/data
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:9000/minio/health/live"]
      interval: 30s
      timeout: 10s
      retries: 3

  minio-init:
    image: minio/mc:latest
    depends_on:
      - minio
    entrypoint: >
      /bin/sh -c "
      mc alias set myminio http://minio:9000 minioadmin minioadmin;
      mc mb myminio/elacsym-data || true;
      mc anonymous set download myminio/elacsym-data;
      exit 0;
      "

volumes:
  minio_data:
```

Start MinIO locally:
```bash
docker-compose up -d
# Access the management UI at http://localhost:9001
```

#### Production MinIO Setup
```bash
# Download and install MinIO server
wget https://dl.min.io/server/minio/release/linux-amd64/minio
chmod +x minio

# Run with a persistent data directory
./minio server /data/minio --console-address ":9001"

# Create the bucket used by Elacsym
mc alias set production http://your-minio-server:9000 ACCESS_KEY SECRET_KEY
mc mb production/elacsym-prod
```

### Multi-Node Configuration

#### Config File: `config.toml`

**Indexer Node 1** (`/etc/elacsym/config-indexer-1.toml`):
```toml
[server]
host = "0.0.0.0"
port = 3000

[storage]
backend = "s3"

[storage.s3]
bucket = "elacsym-data"
region = "us-east-1"
endpoint = "http://minio:9000"
wal_prefix = "production"

[cache]
memory_size = 8589934592        # 8 GiB
disk_size = 107374182400        # 100 GiB
disk_path = "/var/cache/elacsym"

[compaction]
enabled = true
interval_secs = 3600
max_segments = 100
max_total_docs = 1000000

[logging]
level = "info"
format = "json"

[distributed]
enabled = true
node_id = "indexer-1"
role = "indexer"

[distributed.indexer_cluster]
nodes = [
    "indexer-1",
    "indexer-2",
    "indexer-3"
]
```

**Indexer Node 2** (`/etc/elacsym/config-indexer-2.toml`):
```toml
# Same as indexer-1, but override the node ID
[distributed]
enabled = true
node_id = "indexer-2"
role = "indexer"
```

**Query Node** (`/etc/elacsym/config-query-1.toml`):
```toml
[server]
host = "0.0.0.0"
port = 3000

[storage]
backend = "s3"

[storage.s3]
bucket = "elacsym-data"
region = "us-east-1"
endpoint = "http://minio:9000"
wal_prefix = "production"

[cache]
memory_size = 4294967296        # 4 GiB
disk_size = 53687091200         # 50 GiB

[compaction]
enabled = false

[logging]
level = "info"
format = "json"

[distributed]
enabled = true
node_id = "query-1"
role = "query"

[distributed.indexer_cluster]
nodes = [
    "indexer-1",
    "indexer-2",
    "indexer-3"
]
```

#### Environment Variable Overrides
```bash
# Override identity
export ELACSYM_NODE_ID=indexer-1
export ELACSYM_NODE_ROLE=indexer

# Override storage target
export ELACSYM_STORAGE_BACKEND=s3
export ELACSYM_STORAGE_S3_BUCKET=my-bucket
export ELACSYM_STORAGE_S3_REGION=us-west-2

# AWS credentials for production S3
export AWS_ACCESS_KEY_ID=your-key
export AWS_SECRET_ACCESS_KEY=your-secret
```

### Deployment Examples

#### Docker Compose (Complete Cluster)
Create `examples/distributed/docker-compose.yml`:
```yaml
version: '3.8'

services:
  minio:
    image: minio/minio:latest
    command: server /data --console-address ":9001"
    ports:
      - "9000:9000"
      - "9001:9001"
    environment:
      MINIO_ROOT_USER: minioadmin
      MINIO_ROOT_PASSWORD: minioadmin
    volumes:
      - minio_data:/data

  indexer-1:
    image: elacsym:latest
    environment:
      ELACSYM_NODE_ID: indexer-1
      ELACSYM_NODE_ROLE: indexer
      AWS_ACCESS_KEY_ID: minioadmin
      AWS_SECRET_ACCESS_KEY: minioadmin
    volumes:
      - ./config-indexer.toml:/etc/elacsym/config.toml
      - indexer1_cache:/var/cache/elacsym
    ports:
      - "3001:3000"
    depends_on:
      - minio

  indexer-2:
    image: elacsym:latest
    environment:
      ELACSYM_NODE_ID: indexer-2
      ELACSYM_NODE_ROLE: indexer
      AWS_ACCESS_KEY_ID: minioadmin
      AWS_SECRET_ACCESS_KEY: minioadmin
    volumes:
      - ./config-indexer.toml:/etc/elacsym/config.toml
      - indexer2_cache:/var/cache/elacsym
    ports:
      - "3002:3000"
    depends_on:
      - minio

  indexer-3:
    image: elacsym:latest
    environment:
      ELACSYM_NODE_ID: indexer-3
      ELACSYM_NODE_ROLE: indexer
      AWS_ACCESS_KEY_ID: minioadmin
      AWS_SECRET_ACCESS_KEY: minioadmin
    volumes:
      - ./config-indexer.toml:/etc/elacsym/config.toml
      - indexer3_cache:/var/cache/elacsym
    ports:
      - "3003:3000"
    depends_on:
      - minio

  query-1:
    image: elacsym:latest
    environment:
      ELACSYM_NODE_ID: query-1
      ELACSYM_NODE_ROLE: query
      AWS_ACCESS_KEY_ID: minioadmin
      AWS_SECRET_ACCESS_KEY: minioadmin
    volumes:
      - ./config-query.toml:/etc/elacsym/config.toml
      - query1_cache:/var/cache/elacsym
    ports:
      - "3000:3000"
    depends_on:
      - indexer-1
      - indexer-2
      - indexer-3

  nginx:
    image: nginx:alpine
    volumes:
      - ./nginx.conf:/etc/nginx/nginx.conf
    ports:
      - "80:80"
    depends_on:
      - query-1

volumes:
  minio_data:
  indexer1_cache:
  indexer2_cache:
  indexer3_cache:
  query1_cache:
```

#### Nginx Load Balancer Config
Create `examples/distributed/nginx.conf`:
```nginx
events {
    worker_connections 1024;
}

http {
    upstream indexers {
        server indexer-1:3000;
        server indexer-2:3000;
        server indexer-3:3000;
    }

    upstream query_nodes {
        server query-1:3000;
    }

    server {
        listen 80;

        # Route writes to indexers
        location ~ ^/v1/namespaces/.*/upsert$ {
            proxy_pass http://indexers;
            proxy_set_header Host $host;
            proxy_set_header X-Real-IP $remote_addr;
        }

        # Route reads to query nodes
        location ~ ^/v1/namespaces/.*/query$ {
            proxy_pass http://query_nodes;
            proxy_set_header Host $host;
            proxy_set_header X-Real-IP $remote_addr;
        }

        # Default to indexers for control-plane APIs
        location / {
            proxy_pass http://indexers;
            proxy_set_header Host $host;
            proxy_set_header X-Real-IP $remote_addr;
        }
    }
}
```

#### Running the Cluster
```bash
cd examples/distributed

docker-compose up -d

# Health checks
curl http://localhost:3001/health
curl http://localhost:3002/health
curl http://localhost:3003/health
curl http://localhost:3000/health

# Create namespace (automatically routed to responsible indexer)
curl -X PUT http://localhost/v1/namespaces/my-docs \
  -H "Content-Type: application/json" \
  -d '{
    "vector_dim": 768,
    "vector_metric": "cosine",
    "attributes": {
      "title": {"type": "string", "full_text": true}
    }
  }'

# Upsert documents (goes to indexer cluster)
curl -X POST http://localhost/v1/namespaces/my-docs/upsert \
  -H "Content-Type: application/json" \
  -d '{
    "documents": [
      {
        "id": 1,
        "vector": [0.1, 0.2, 0.3],
        "attributes": {"title": "Hello World"}
      }
    ]
  }'

# Query via query nodes
curl -X POST http://localhost/v1/namespaces/my-docs/query \
  -H "Content-Type: application/json" \
  -d '{
    "vector": [0.1, 0.2, 0.3],
    "top_k": 10
  }'
```

#### Kubernetes Deployment
The `examples/distributed/k8s/` directory contains:
- `indexer-statefulset.yaml`
- `query-deployment.yaml`
- `service.yaml`
- `ingress.yaml`

The manifests follow standard Kubernetes best practices—StatefulSets for indexers (stable identities), Deployments for query nodes, a headless Service for internal traffic, and an Ingress or Gateway for external requests. Customize resource requests to match workload requirements.

## Production Best Practices
- Use dedicated MinIO/S3 buckets per environment to isolate data.
- Enable TLS termination at the load balancer (Nginx, AWS ALB, or service mesh).
- Configure structured logging (`logging.format = "json"`) for log aggregation systems.
- Monitor WAL cleanup jobs—stale files indicate rotation issues or S3 permissions.
- Regularly validate backups of manifests and WAL directories.

## Troubleshooting

### Issue: `node_id not found in indexer_cluster.nodes`
**Cause**: Node ID does not match the configured cluster list.

**Solution**:
```bash
echo $ELACSYM_NODE_ID
grep -A3 "indexer_cluster" /etc/elacsym/config.toml
```
Ensure the node ID appears in every node's `distributed.indexer_cluster.nodes` array.

### Issue: `Role mismatch: node configured as Query but processing as Indexer`
**Cause**: Environment override (`ELACSYM_NODE_ROLE`) disagrees with config.

**Solution**:
```toml
[distributed]
role = "query"  # must match ELACSYM_NODE_ROLE or remove the override
```

### Issue: WAL files are not cleaned up
**Cause**: Insufficient S3 permissions or network failures during deletion.

**Solution**:
```bash
aws s3 ls s3://your-bucket/wal/
grep wal_prefix /etc/elacsym/config.toml
```
Verify IAM policy allows `s3:DeleteObject` and confirm `wal_prefix` alignment across nodes.

### Issue: Namespace routing errors
**Cause**: Indexer cluster list differs between nodes, leading to inconsistent hashing.

**Solution**:
```bash
grep -A5 "indexer_cluster" /etc/elacsym/config*.toml
```
Ensure all nodes share the same ordered list of indexer IDs.

## Performance Tuning

### Cache Sizing
- **Indexer nodes**: 8–16 GiB memory cache, 100–500 GiB disk cache depending on dataset size.
- **Query nodes**: 4–8 GiB memory cache, 50–100 GiB disk cache—lower write amplification.

### Compaction Settings
```toml
[compaction]
interval_secs = 1800      # Increase frequency for heavy write workloads
max_segments = 50          # Lower thresholds improve query latency
max_total_docs = 500000    # Tune based on document size and query SLA
```

### Network Considerations
- Co-locate indexers and MinIO/S3 in the same availability zone to minimize latency.
- Use private networking for node ↔ MinIO traffic; avoid traversing the public internet.
- Monitor S3 request latency (target <20 ms p99) and enable retry policies on transient failures.
