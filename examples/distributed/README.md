# Distributed Deployment Examples

This directory contains reference assets for running Elacsym in distributed mode.

## Contents
- `config-indexer.toml` – shared configuration template for indexer nodes
- `config-query.toml` – configuration template for query nodes
- `docker-compose.yml` – spin up MinIO, three indexers, one query node, and Nginx
- `nginx.conf` – HTTP routing example that separates reads/writes
- `.env.example` – environment variables for Docker Compose
- `k8s/` – starter Kubernetes manifests (StatefulSet, Deployment, Services, Ingress)

## Quick Start (Docker Compose)

```bash
cp .env.example .env
# Optional: customize credentials/bucket names

# Launch the cluster
docker-compose up -d

# Verify health
curl http://localhost:3001/health   # indexer-1
curl http://localhost:3000/health   # query-1 via load balancer
```

Create a namespace and index data:

```bash
curl -X PUT http://localhost/v1/namespaces/demo \
  -H "Content-Type: application/json" \
  -d '{
    "vector_dim": 768,
    "vector_metric": "cosine",
    "attributes": {
      "title": {"type": "string", "full_text": true}
    }
  }'

curl -X POST http://localhost/v1/namespaces/demo/upsert \
  -H "Content-Type: application/json" \
  -d '{
    "documents": [
      {
        "id": 1,
        "vector": [0.1, 0.2, 0.3],
        "attributes": {"title": "Hello Distributed Elacsym"}
      }
    ]
  }'

curl -X POST http://localhost/v1/namespaces/demo/query \
  -H "Content-Type: application/json" \
  -d '{
    "vector": [0.1, 0.2, 0.3],
    "top_k": 5
  }'
```

Refer to [docs/deployment.md](../../docs/deployment.md#distributed-deployment) for a full walkthrough, production guidance, and troubleshooting tips.
