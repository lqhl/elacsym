# Performance Tuning Guide

This guide helps you optimize Elacsym for your workload.

## Performance Goals

Target latencies for common operations:

| Operation | Hot (cache hit) | Cold (cache miss) | Target Throughput |
|-----------|----------------|-------------------|-------------------|
| Vector search (1M vectors) | < 20ms | < 500ms | 1000 qps |
| Full-text search | < 50ms | < 600ms | 500 qps |
| Hybrid search | < 100ms | < 800ms | 300 qps |
| Upsert (single doc) | 5-10ms | N/A | N/A |
| Upsert (batch 1000) | 100-200ms | N/A | 5000-10000 docs/s |

## Cache Tuning

### Memory Cache Sizing

**Formula**:
```
memory_cache = (num_namespaces × manifest_size) + (num_hot_indexes × index_size)

Where:
  manifest_size ≈ 1 MB per namespace
  index_size ≈ 0.01 MB per 1000 vectors (RaBitQ compressed)
```

**Example**:
```
10 namespaces × 1 MB = 10 MB
1M vectors × 0.01 MB / 1000 = 10 MB
Total: 20 MB (actual: set to 1-2 GB for headroom)
```

**Recommendations**:
- Small workload (< 1M vectors): 1-2 GB
- Medium workload (1M-10M vectors): 4-8 GB
- Large workload (> 10M vectors): 8-16 GB

### Disk Cache Sizing

**Formula**:
```
disk_cache = hot_data_size × 1.5

Where:
  hot_data_size = frequently queried segments
  Rule of thumb: 10-20% of total data
```

**Example** (10M vectors, 768-dim, 10% hot):
```
Total data: 10M × 768 × 4 bytes = 30 GB
Hot data (10%): 3 GB
Disk cache: 3 GB × 1.5 = 4.5 GB (set to 10-50 GB for buffer)
```

**Recommendations**:
| Total Data | Hot Data (10%) | Disk Cache |
|------------|---------------|------------|
| 10 GB | 1 GB | 10-50 GB |
| 100 GB | 10 GB | 50-200 GB |
| 1 TB | 100 GB | 200-500 GB |
| 10 TB | 1 TB | 1-2 TB |

### Cache Hit Rate Monitoring

**Check logs for cache misses**:
```bash
# Memory cache misses
grep "cache miss" /var/log/elacsym.log | grep "memory" | wc -l

# Disk cache misses
grep "cache miss" /var/log/elacsym.log | grep "disk" | wc -l
```

**Target hit rates**:
- Memory cache: > 80%
- Disk cache: > 90%

**If hit rate is low**:
- Increase cache size
- Reduce number of namespaces per node
- Use query result caching (P2 feature)

## Compaction Tuning

### Trigger Thresholds

**Default**:
```toml
[compaction]
max_segments = 100
max_total_docs = 1000000
```

**High write rate** (< 100 segments is better):
```toml
max_segments = 50      # More frequent compaction
max_total_docs = 500000
```

**Low write rate** (reduce overhead):
```toml
max_segments = 200     # Less frequent compaction
max_total_docs = 2000000
```

### Compaction Interval

**Default**: Check every 1 hour

**High write rate**:
```toml
interval_secs = 1800  # 30 minutes
```

**Low write rate**:
```toml
interval_secs = 7200  # 2 hours
```

**Trade-off**:
- **Lower interval**: Fewer segments → faster queries, but more CPU/memory usage
- **Higher interval**: Less overhead, but slower queries (more segments to scan)

### Monitoring Compaction

**Check segment count**:
```bash
# Via API (if metrics endpoint exists)
curl http://localhost:3000/metrics | grep segment_count

# Via logs
grep "compaction triggered" /var/log/elacsym.log
```

**Compaction is working if**:
- Segment count stays below threshold
- Query latency doesn't degrade over time

## Query Optimization

### Vector Search

**1. Choose Right Metric**:
- **Cosine**: Normalized embeddings (e.g., OpenAI, Sentence Transformers)
- **L2**: General purpose, unnormalized vectors
- **Dot Product**: Pre-normalized vectors, slightly faster than cosine

**2. Tune top_k**:
- Higher top_k → slower query (more results to process)
- Recommended: 10-50 for most use cases
- If you need 100+ results: Consider pagination or narrower filters

**3. Use Filters**:
- Filter early to reduce search space
- Example: Filter by `date > 2020` before vector search

### Full-Text Search

**1. Use Stemming**:
```json
"full_text": {
  "language": "english",
  "stemming": true,
  "remove_stopwords": true
}
```
Reduces index size and improves recall ("running" matches "run").

**2. Multi-Field Search with Weights**:
```json
"full_text": {
  "fields": ["title", "body"],
  "query": "machine learning",
  "weights": {
    "title": 3.0,    # Title matches 3x more important
    "body": 1.0
  }
}
```

**3. Limit Fields**:
- Only enable full-text on fields you'll search
- Each full-text field adds ~5-10MB per 100k docs

### Hybrid Search

**RRF Fusion is expensive** (2× searches + merge):
- Only use when you need both semantic and keyword matching
- Consider running separate queries if you only need one type

## Write Optimization

### Batch Upserts

**Bad** (1000 individual requests):
```bash
for i in {1..1000}; do
  curl -X POST .../upsert -d '{"documents":[{"id":'$i',...}]}'
done
```

**Good** (single batch):
```bash
curl -X POST .../upsert -d '{
  "documents": [
    {"id":1,...},
    {"id":2,...},
    ...
    {"id":1000,...}
  ]
}'
```

**Speedup**: 10-100x (amortized WAL fsync, index rebuild)

**Recommended batch size**: 100-1000 documents

### Parallel Writes (Distributed)

With 3 indexer nodes:
- Each namespace assigned to one indexer
- Write to different namespaces in parallel
- 3× write throughput

**Sharding**:
```python
# Pseudo-code
for namespace in namespaces:
    indexer = hash(namespace) % num_indexers
    write_to(indexer, namespace, documents)
```

## Storage Optimization

### S3 Transfer Acceleration

For cross-region deployments:
```bash
aws s3api put-bucket-accelerate-configuration \
  --bucket elacsym-production \
  --accelerate-configuration Status=Enabled
```

Speedup: 50-500% for long-distance transfers

### S3 Intelligent-Tiering

Automatically move cold data to cheaper storage:
```bash
aws s3api put-bucket-intelligent-tiering-configuration \
  --bucket elacsym-production \
  --id rule1 \
  --intelligent-tiering-configuration '{
    "Id": "rule1",
    "Status": "Enabled",
    "Tierings": [
      {"Days": 90, "AccessTier": "ARCHIVE_ACCESS"},
      {"Days": 180, "AccessTier": "DEEP_ARCHIVE_ACCESS"}
    ]
  }'
```

Savings: 50-90% for infrequently accessed namespaces

### Local Storage

**Use fast disk**:
- NVMe SSD: Best performance
- SATA SSD: Good performance
- HDD: Slow, not recommended

**Filesystem**:
- **ext4**: Good general purpose
- **xfs**: Better for large files (> 1GB segments)
- **btrfs**: Compression support (not tested)

## Network Optimization

### Same Region as S3

**Latency**:
- Same region: 5-20 ms
- Cross-region: 50-200 ms
- Cross-continent: 200-500 ms

**Always deploy in same region as S3 bucket.**

### Bandwidth

**Minimum**: 1 Gbps (100 MB/s)
- Can handle 100-200 queries/sec

**Recommended**: 10 Gbps (1 GB/s)
- Can handle 1000+ queries/sec

**Check bottlenecks**:
```bash
# Network usage
iftop

# S3 transfer speed
aws s3 cp s3://bucket/large-file - | pv > /dev/null
```

## Benchmarking

### Load Testing

Use tools like `wrk` or `k6`:

**Vector search benchmark**:
```bash
wrk -t12 -c400 -d30s --latency \
  -s query.lua \
  http://localhost:3000/v1/namespaces/test/query
```

**query.lua**:
```lua
request = function()
  local vector = {}
  for i = 1, 768 do
    vector[i] = math.random()
  end

  local body = '{"vector":' .. json_encode(vector) .. ',"top_k":10}'
  return wrk.format("POST", nil, nil, body)
end
```

**Expected results** (1M vectors, good hardware):
```
Latency Distribution:
  50%: 15ms
  75%: 25ms
  90%: 50ms
  99%: 200ms

Throughput: 800-1000 req/sec
```

### Profiling

**CPU profiling**:
```bash
cargo build --release
perf record --call-graph dwarf ./target/release/elacsym
perf report
```

**Memory profiling**:
```bash
valgrind --tool=massif ./target/release/elacsym
ms_print massif.out.*
```

## Hardware Recommendations

### CPU

**Single Node**:
- Minimum: 4 cores (Intel Xeon, AMD EPYC, or equivalent)
- Recommended: 8-16 cores
- **More cores = more concurrent queries**

### Memory

**Formula**:
```
total_ram = cache.memory_size + 8 GB (system + buffers)
```

**Examples**:
| Cache Size | Total RAM |
|------------|-----------|
| 4 GB | 12 GB |
| 8 GB | 16 GB |
| 16 GB | 24 GB |
| 32 GB | 40 GB |

### Disk (for cache)

**Type**:
- **Best**: NVMe SSD (3000+ MB/s read)
- **Good**: SATA SSD (500+ MB/s read)
- **Avoid**: HDD (< 100 MB/s read)

**Size**: See [Disk Cache Sizing](#disk-cache-sizing)

**IOPS**:
- Minimum: 1000 IOPS
- Recommended: 5000+ IOPS

### Network

**Bandwidth**:
- Minimum: 1 Gbps
- Recommended: 10 Gbps

**Latency to S3**:
- Same region: < 20 ms
- Cross-region: Avoid if possible

## Cost Optimization

### S3 vs Memory Cost Comparison

**AWS us-east-1 pricing**:
- S3 Standard: $0.023/GB/month
- EC2 memory (r6i.4xlarge): ~$2-4/GB/month
- **Savings: 87-99%** for data in S3 vs memory

**Example** (1M vectors, 768-dim):
```
Data size: 3 GB
In-memory (EC2): $6-12/month
With Elacsym:
  - S3: 3 GB × $0.023 = $0.07/month
  - Indexes (memory): 30 MB × $3 = $0.09/month
  - Total: $0.16/month
Savings: 97-99%
```

### Tips

1. **Use S3 Intelligent-Tiering** for cold data
2. **Set S3 Lifecycle Policies** to delete old WAL files
3. **Compress full-text indexes** (enabled by default)
4. **Right-size cache**: Don't over-provision
5. **Use Spot Instances** (Kubernetes + node groups)

## Monitoring

### Key Metrics (Coming in P1)

Once Prometheus metrics are available:

1. **Query Latency** (p50, p95, p99)
   - Target: p50 < 50ms, p99 < 500ms

2. **Cache Hit Rate**
   - Target: > 80% (memory), > 90% (disk)

3. **Compaction Frequency**
   - Should stabilize (not growing)

4. **Segment Count**
   - Should stay below threshold

5. **Write Throughput**
   - Baseline: 1000-5000 docs/s

### Log Analysis

**Slow query log**:
```bash
# Find queries > 1s
grep "query took" /var/log/elacsym.log | awk '{if ($NF > 1000) print}'
```

**Cache miss rate**:
```bash
total=$(grep "cache" /var/log/elacsym.log | wc -l)
misses=$(grep "cache miss" /var/log/elacsym.log | wc -l)
echo "Hit rate: $((100 - misses * 100 / total))%"
```

## Troubleshooting Performance

### High Query Latency

**Diagnose**:
1. Check cache hit rate
2. Check network latency to S3
3. Check CPU usage
4. Check segment count

**Solutions**:
- Increase cache size
- Trigger manual compaction
- Add more query nodes (distributed mode)
- Use faster disk for cache

### Low Write Throughput

**Diagnose**:
1. Check if single-doc vs batch upserts
2. Check compaction frequency
3. Check WAL fsync time

**Solutions**:
- Use batch upserts (100-1000 docs)
- Reduce compaction frequency
- Use faster disk for WAL
- Use multiple indexer nodes

### High Memory Usage

**Diagnose**:
1. Check cache.memory_size
2. Check for memory leaks (restart server, monitor)
3. Check number of namespaces

**Solutions**:
- Reduce cache.memory_size
- Reduce number of namespaces per node
- Use distributed mode (shard namespaces)

## Best Practices

1. **Start small, scale up**: Begin with conservative cache sizes, increase based on metrics
2. **Monitor continuously**: Set up health checks and alerting
3. **Batch writes**: Always batch when possible (10-100× speedup)
4. **Use filters**: Reduce search space before vector search
5. **Tune compaction**: Find the right balance for your write rate
6. **Test in production-like environment**: Benchmark with real data and queries
7. **Keep segments small**: Lower compaction thresholds for better query performance
8. **Use same AWS region**: Minimize latency to S3

## Next Steps

- [Configuration](configuration.md) - Detailed config options
- [Deployment](deployment.md) - Production setup
- [Architecture](architecture.md) - Understand the system
