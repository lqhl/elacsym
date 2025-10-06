# Deployment Guide

This guide covers deploying Elacsym in production environments.

## Quick Start

### Single Node (Local Storage)

```bash
# Build release binary
cargo build --release

# Create data directory
mkdir -p /var/lib/elacsym /var/cache/elacsym

# Run server
ELACSYM_STORAGE_PATH=/var/lib/elacsym \
ELACSYM_CACHE_DISK_PATH=/var/cache/elacsym \
./target/release/elacsym
```

### Single Node (S3 Storage)

```bash
# Configure AWS credentials
export AWS_ACCESS_KEY_ID=your_access_key
export AWS_SECRET_ACCESS_KEY=your_secret_key

# Run server
ELACSYM_STORAGE_BACKEND=s3 \
ELACSYM_S3_BUCKET=elacsym-production \
ELACSYM_S3_REGION=us-west-2 \
./target/release/elacsym
```

## System Requirements

### Minimum (Development)
- CPU: 2 cores
- RAM: 4GB
- Disk: 20GB SSD
- Network: 1 Gbps

### Recommended (Production)
- CPU: 8+ cores
- RAM: 16GB+ (8GB cache + 8GB system)
- Disk: 500GB+ NVMe SSD (for cache)
- Network: 10 Gbps

### Scaling Guidelines

| Vectors | Documents | Memory Cache | Disk Cache | CPU Cores | Notes |
|---------|-----------|--------------|------------|-----------|-------|
| < 1M | < 1M | 2GB | 50GB | 4 | Single node sufficient |
| 1M - 10M | 1M - 10M | 8GB | 200GB | 8 | Single node or 2-3 indexers |
| 10M - 100M | 10M - 100M | 16GB | 1TB | 16 | 3-5 indexer nodes |
| > 100M | > 100M | 32GB+ | 2TB+ | 32+ | 5+ indexer nodes + dedicated query nodes |

## Storage Backend

### Local Filesystem

**Pros**:
- Fast (no network latency)
- Simple setup
- No external dependencies

**Cons**:
- Not distributed
- Limited to single node capacity
- No built-in replication

**Use Cases**:
- Development
- Small datasets (< 1M vectors)
- Edge deployments

**Setup**:
```bash
mkdir -p /var/lib/elacsym
chown elacsym:elacsym /var/lib/elacsym
chmod 750 /var/lib/elacsym
```

### AWS S3

**Pros**:
- Cheap ($0.023/GB/month vs $2-4/GB/month for RAM)
- Unlimited capacity
- Built-in durability (99.999999999%)
- Multi-region support

**Cons**:
- Higher latency (mitigated by caching)
- Requires AWS account

**Setup**:

1. **Create S3 Bucket**:
```bash
aws s3 mb s3://elacsym-production --region us-west-2
```

2. **Set Lifecycle Policy** (optional, for old WAL cleanup):
```json
{
  "Rules": [{
    "Id": "DeleteOldWAL",
    "Status": "Enabled",
    "Filter": {"Prefix": "*/wal/"},
    "Expiration": {"Days": 7}
  }]
}
```

3. **Create IAM User**:
```bash
aws iam create-user --user-name elacsym-production
```

4. **Attach Policy**:
```json
{
  "Version": "2012-10-17",
  "Statement": [{
    "Effect": "Allow",
    "Action": [
      "s3:GetObject",
      "s3:PutObject",
      "s3:DeleteObject",
      "s3:ListBucket"
    ],
    "Resource": [
      "arn:aws:s3:::elacsym-production",
      "arn:aws:s3:::elacsym-production/*"
    ]
  }]
}
```

5. **Generate Access Keys**:
```bash
aws iam create-access-key --user-name elacsym-production
```

### MinIO (Self-Hosted S3)

**Pros**:
- S3-compatible (no code changes)
- Self-hosted (no AWS account needed)
- Lower latency than public cloud

**Setup**:

1. **Run MinIO**:
```bash
docker run -d \
  -p 9000:9000 -p 9001:9001 \
  -e "MINIO_ROOT_USER=admin" \
  -e "MINIO_ROOT_PASSWORD=password" \
  -v /data/minio:/data \
  minio/minio server /data --console-address ":9001"
```

2. **Create Bucket**:
```bash
mc alias set myminio http://localhost:9000 admin password
mc mb myminio/elacsym
```

3. **Configure Elacsym**:
```toml
[storage]
backend = "s3"

[storage.s3]
bucket = "elacsym"
region = "us-east-1"  # Any value works
endpoint = "http://localhost:9000"
```

## Deployment Architectures

### Architecture 1: Single Node

```
┌─────────────────────────────────────┐
│         Client Applications         │
└─────────────────────────────────────┘
                  ↓
         ┌───────────────┐
         │  Load Balancer│ (optional)
         └───────────────┘
                  ↓
    ┌──────────────────────────┐
    │    Elacsym Server        │
    │  (Indexer + Query Node)  │
    │                          │
    │  - Cache: 8GB + 200GB    │
    │  - Storage: S3           │
    └──────────────────────────┘
```

**Pros**: Simple, easy to manage
**Cons**: Single point of failure, limited write throughput
**Best for**: < 10M vectors, < 1000 qps

### Architecture 2: Distributed (3 Indexers + 2 Query Nodes)

```
┌─────────────────────────────────────┐
│         Client Applications         │
└─────────────────────────────────────┘
                  ↓
         ┌───────────────┐
         │  Load Balancer│
         └───────────────┘
          ↓       ↓      ↓
    ┌─────┴──┐  ┌┴──────┴──────┐
    │ Query  │  │   Query Node  │
    │ Node 1 │  │   Node 2      │
    └────────┘  └───────────────┘
      ↓   ↓        ↓
      ↓   └────┬───┘
      ↓        ↓
  ┌───────────────────────────────────┐
  │     Indexer Nodes (Writes)        │
  │  ┌─────────┬─────────┬─────────┐  │
  │  │Indexer 0│Indexer 1│Indexer 2│  │
  │  └─────────┴─────────┴─────────┘  │
  └───────────────────────────────────┘
                  ↓
         ┌───────────────┐
         │   S3 Storage  │
         │  (Shared)     │
         └───────────────┘
```

**Pros**: High availability, write sharding, read scaling
**Cons**: More complex, requires coordination
**Best for**: > 10M vectors, > 1000 qps

**Configuration**:

**Indexer Node 0**:
```toml
[distributed]
enabled = true
node_id = "indexer-0"
role = "indexer"

[distributed.indexer_cluster]
nodes = ["indexer-0", "indexer-1", "indexer-2"]
```

**Query Node**:
```toml
[distributed]
enabled = true
node_id = "query-0"
role = "query"

[distributed.indexer_cluster]
nodes = ["indexer-0", "indexer-1", "indexer-2"]
```

## Docker Deployment

### Dockerfile

```dockerfile
FROM rust:1.75 as builder

WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Create user
RUN useradd -m -u 1000 elacsym

# Copy binary
COPY --from=builder /app/target/release/elacsym /usr/local/bin/

# Create directories
RUN mkdir -p /var/lib/elacsym /var/cache/elacsym && \
    chown -R elacsym:elacsym /var/lib/elacsym /var/cache/elacsym

USER elacsym
WORKDIR /home/elacsym

EXPOSE 3000

CMD ["elacsym"]
```

### Docker Compose (Single Node)

```yaml
version: '3.8'

services:
  elacsym:
    build: .
    ports:
      - "3000:3000"
    environment:
      - ELACSYM_STORAGE_BACKEND=local
      - ELACSYM_STORAGE_PATH=/data
      - ELACSYM_CACHE_MEMORY_SIZE=4294967296
      - ELACSYM_CACHE_DISK_SIZE=107374182400
      - RUST_LOG=info
    volumes:
      - elacsym-data:/data
      - elacsym-cache:/cache
    restart: unless-stopped

volumes:
  elacsym-data:
  elacsym-cache:
```

### Docker Compose (with MinIO)

```yaml
version: '3.8'

services:
  minio:
    image: minio/minio
    ports:
      - "9000:9000"
      - "9001:9001"
    environment:
      - MINIO_ROOT_USER=admin
      - MINIO_ROOT_PASSWORD=password123
    volumes:
      - minio-data:/data
    command: server /data --console-address ":9001"

  elacsym:
    build: .
    depends_on:
      - minio
    ports:
      - "3000:3000"
    environment:
      - ELACSYM_STORAGE_BACKEND=s3
      - ELACSYM_S3_BUCKET=elacsym
      - ELACSYM_S3_REGION=us-east-1
      - ELACSYM_S3_ENDPOINT=http://minio:9000
      - AWS_ACCESS_KEY_ID=admin
      - AWS_SECRET_ACCESS_KEY=password123
      - RUST_LOG=info
    volumes:
      - elacsym-cache:/cache

volumes:
  minio-data:
  elacsym-cache:
```

Run:
```bash
docker-compose up -d
```

## Kubernetes Deployment

### StatefulSet (Single Node)

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: elacsym-config
data:
  config.toml: |
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
    disk_path = "/cache"

---
apiVersion: v1
kind: Secret
metadata:
  name: aws-credentials
type: Opaque
stringData:
  AWS_ACCESS_KEY_ID: your_access_key
  AWS_SECRET_ACCESS_KEY: your_secret_key

---
apiVersion: apps/v1
kind: StatefulSet
metadata:
  name: elacsym
spec:
  serviceName: elacsym
  replicas: 1
  selector:
    matchLabels:
      app: elacsym
  template:
    metadata:
      labels:
        app: elacsym
    spec:
      containers:
      - name: elacsym
        image: elacsym:latest
        ports:
        - containerPort: 3000
          name: http
        env:
        - name: AWS_ACCESS_KEY_ID
          valueFrom:
            secretKeyRef:
              name: aws-credentials
              key: AWS_ACCESS_KEY_ID
        - name: AWS_SECRET_ACCESS_KEY
          valueFrom:
            secretKeyRef:
              name: aws-credentials
              key: AWS_SECRET_ACCESS_KEY
        volumeMounts:
        - name: config
          mountPath: /etc/elacsym
        - name: cache
          mountPath: /cache
        resources:
          requests:
            memory: "16Gi"
            cpu: "4"
          limits:
            memory: "24Gi"
            cpu: "8"
        livenessProbe:
          httpGet:
            path: /health
            port: 3000
          initialDelaySeconds: 30
          periodSeconds: 10
        readinessProbe:
          httpGet:
            path: /health
            port: 3000
          initialDelaySeconds: 5
          periodSeconds: 5
      volumes:
      - name: config
        configMap:
          name: elacsym-config
  volumeClaimTemplates:
  - metadata:
      name: cache
    spec:
      accessModes: ["ReadWriteOnce"]
      storageClassName: fast-ssd
      resources:
        requests:
          storage: 500Gi

---
apiVersion: v1
kind: Service
metadata:
  name: elacsym
spec:
  selector:
    app: elacsym
  ports:
  - port: 3000
    targetPort: 3000
  type: LoadBalancer
```

### Distributed Deployment (3 Indexers)

See `examples/kubernetes/distributed.yaml` (to be added).

## Monitoring

### Health Checks

```bash
# Liveness probe
curl http://localhost:3000/health

# Expected response
{"status":"healthy","version":"0.1.0","namespaces":5}
```

### Logs

**Structured JSON logs** (recommended for production):
```bash
RUST_LOG=info ./elacsym 2>&1 | jq
```

**Send to logging system**:
```bash
# Fluent Bit
./elacsym 2>&1 | fluent-bit -c fluent-bit.conf

# Logstash
./elacsym 2>&1 | logstash -f logstash.conf

# CloudWatch Logs (AWS)
./elacsym 2>&1 | aws logs put-log-events ...
```

### Metrics (Coming in P1)

Future: Prometheus `/metrics` endpoint

```
# HELP elacsym_query_duration_seconds Query duration
# TYPE elacsym_query_duration_seconds histogram
elacsym_query_duration_seconds_bucket{le="0.01"} 1000
elacsym_query_duration_seconds_bucket{le="0.05"} 5000
...

# HELP elacsym_cache_hit_rate Cache hit rate
# TYPE elacsym_cache_hit_rate gauge
elacsym_cache_hit_rate{layer="memory"} 0.85
elacsym_cache_hit_rate{layer="disk"} 0.92
```

## Backup and Recovery

### Backup Strategy

**What to backup**:
- Manifests (critical): `{namespace}/manifest.json`
- Segments (critical): `{namespace}/segments/*.parquet`
- Indexes (can rebuild): `{namespace}/indexes/*`
- WAL (temporary): Not needed in backups

**S3 Backup** (automatic):
- S3 has 99.999999999% durability
- Enable versioning for accidental deletion protection:
  ```bash
  aws s3api put-bucket-versioning \
    --bucket elacsym-production \
    --versioning-configuration Status=Enabled
  ```

**Local Storage Backup**:
```bash
# Full backup
tar czf elacsym-backup-$(date +%Y%m%d).tar.gz /var/lib/elacsym

# Incremental backup (rsync)
rsync -av --delete /var/lib/elacsym /backup/elacsym
```

### Disaster Recovery

**Scenario 1: Corrupted WAL**
- Automatic: WAL recovery skips corrupted entries
- Manual: Delete WAL files (safe when server stopped)

**Scenario 2: Lost Indexes**
- Indexes are derived data (can rebuild)
- Restart server: Indexes rebuild automatically from segments

**Scenario 3: Lost Segments**
- If using S3: Restore from S3 versioning
- If using local: Restore from backup
- Worst case: Data loss for affected segments

**Scenario 4: Corrupted Manifest**
- If using S3: Restore from S3 versioning
- If using local: Restore previous version manually
- Manifest contains segment list and schema

## Security

### Network Security

1. **Firewall Rules**:
```bash
# Allow only from application servers
iptables -A INPUT -p tcp --dport 3000 -s 10.0.0.0/8 -j ACCEPT
iptables -A INPUT -p tcp --dport 3000 -j DROP
```

2. **TLS Termination** (use reverse proxy):
```nginx
server {
    listen 443 ssl;
    server_name elacsym.example.com;

    ssl_certificate /etc/ssl/certs/elacsym.crt;
    ssl_certificate_key /etc/ssl/private/elacsym.key;

    location / {
        proxy_pass http://localhost:3000;
    }
}
```

### Authentication

**Current**: No built-in authentication (v0.1.0)

**Workaround**: Use API gateway or reverse proxy

**nginx with API Key**:
```nginx
location /v1/ {
    if ($http_x_api_key != "secret_key_here") {
        return 401;
    }
    proxy_pass http://localhost:3000;
}
```

**AWS API Gateway**: Use API keys and usage plans

**Future** (P2): JWT tokens, mTLS

### Data Encryption

**At Rest**:
- S3: Enable S3 encryption (SSE-S3 or SSE-KMS)
- Local: Use encrypted disk (LUKS, dm-crypt)

**In Transit**:
- S3: HTTPS by default
- Client ↔ Elacsym: Use TLS termination proxy

## Troubleshooting

### Server Won't Start

**Check**:
```bash
# Port in use
lsof -i :3000

# Storage permissions
ls -la /var/lib/elacsym

# S3 connectivity
aws s3 ls s3://your-bucket

# View logs
RUST_LOG=debug ./elacsym
```

### High Latency

**Diagnose**:
```bash
# Check cache hit rate (look for "cache miss" in logs)
grep "cache miss" /var/log/elacsym.log | wc -l

# Check S3 latency
aws s3 cp s3://your-bucket/test test --region us-west-2

# Check disk I/O
iostat -x 1
```

**Solutions**:
- Increase cache size
- Use faster disk for cache (NVMe)
- Move to same AWS region as S3 bucket

### Out of Memory

**Diagnose**:
```bash
# Check memory usage
docker stats elacsym

# Check cache size
du -sh /var/cache/elacsym
```

**Solutions**:
- Reduce `cache.memory_size`
- Increase system RAM
- Enable swap (not recommended for production)

## Production Checklist

Before going to production:

- [ ] Use S3 or distributed storage (not local FS)
- [ ] Enable S3 versioning for recovery
- [ ] Set up monitoring (health checks, logs)
- [ ] Configure backups (S3 automatic, or cron for local)
- [ ] Use TLS (reverse proxy)
- [ ] Add authentication (API gateway or proxy)
- [ ] Set resource limits (Docker/Kubernetes)
- [ ] Test disaster recovery
- [ ] Load test your workload
- [ ] Set up alerting (on health check failures)
- [ ] Document runbooks for common issues

## Next Steps

- [Configuration](configuration.md) - Tune performance settings
- [Performance](performance.md) - Optimize for your workload
- [API Reference](api-reference.md) - Integrate with your application
