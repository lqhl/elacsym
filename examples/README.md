# Examples

This directory contains example applications demonstrating elacsym usage.

## Running Examples

### Simple Server

Start the elacsym server:

```bash
cargo run --bin elacsym-server
```

The server will start on `http://0.0.0.0:8080` with the following endpoints:

- `GET /health` - Health check
- `POST /namespaces` - Create a namespace
- `POST /vectors/upsert` - Insert or update vectors
- `POST /vectors/query` - Query for similar vectors
- `POST /vectors/fetch` - Fetch vectors by ID
- `POST /vectors/delete` - Delete vectors

### Client Example

Run the client example (requires the server to be running):

```bash
cargo run --example client_example
```

This will demonstrate:
1. Health check
2. Creating a namespace
3. Upserting vectors
4. Querying for similar vectors
5. Fetching specific vectors
6. Deleting vectors

## Manual Testing with curl

### Create a Namespace

```bash
curl -X POST http://localhost:8080/namespaces \
  -H "Content-Type: application/json" \
  -d '{
    "name": "test",
    "dimension": 128
  }'
```

### Upsert Vectors

```bash
curl -X POST http://localhost:8080/vectors/upsert \
  -H "Content-Type: application/json" \
  -d '{
    "vectors": [
      {
        "id": "vec1",
        "values": [0.1, 0.2, ... ],
        "metadata": {"category": "test"}
      }
    ],
    "namespace": "test"
  }'
```

### Query Vectors

```bash
curl -X POST http://localhost:8080/vectors/query \
  -H "Content-Type: application/json" \
  -d '{
    "vector": [0.1, 0.2, ... ],
    "top_k": 10,
    "namespace": "test",
    "include_metadata": true
  }'
```
