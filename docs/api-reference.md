# API Reference

Elacsym provides a RESTful HTTP API for managing namespaces and documents. All endpoints use JSON for request and response bodies.

## Base URL

```
http://<host>:<port>/v1
```

Default: `http://localhost:3000/v1`

## Endpoints

### Health Check

Check if the server is running and healthy.

```http
GET /health
```

**Response** (200 OK):
```json
{
  "status": "healthy",
  "version": "0.1.0",
  "namespaces": 5
}
```

**Fields**:
- `status`: Always "healthy" if server is responding
- `version`: Server version from Cargo.toml
- `namespaces`: Number of active namespaces

---

### Create Namespace

Create a new namespace with a schema definition.

```http
PUT /v1/namespaces/{namespace}
Content-Type: application/json
```

**Path Parameters**:
- `namespace`: Namespace name (alphanumeric + underscore/hyphen)

**Request Body**:
```json
{
  "schema": {
    "vector_dim": 768,
    "vector_metric": "cosine",
    "attributes": {
      "title": {
        "type": "string",
        "indexed": false,
        "full_text": {
          "language": "english",
          "stemming": true,
          "remove_stopwords": true,
          "case_sensitive": false
        }
      },
      "category": {
        "type": "string",
        "indexed": true,
        "full_text": false
      },
      "score": {
        "type": "float",
        "indexed": true
      },
      "published": {
        "type": "bool",
        "indexed": false
      },
      "tags": {
        "type": "array_string",
        "indexed": false
      }
    }
  }
}
```

**Schema Fields**:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `vector_dim` | integer | Yes | Dimension of embedding vectors (e.g., 768, 1536) |
| `vector_metric` | string | Yes | Distance metric: `"cosine"`, `"l2"`, or `"dot"` |
| `attributes` | object | Yes | Map of attribute name to AttributeSchema |

**AttributeSchema Fields**:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `type` | string | Yes | `"string"`, `"int"`, `"float"`, `"bool"`, `"array_string"` |
| `indexed` | boolean | No | Build index for fast filtering (default: false) |
| `full_text` | bool or object | No | Enable full-text search (default: false) |

**FullTextConfig** (when `full_text` is object):

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `language` | string | `"english"` | Language for stemming/stopwords (see [Supported Languages](#supported-languages)) |
| `stemming` | boolean | `true` | Apply stemming (e.g., "running" → "run") |
| `remove_stopwords` | boolean | `true` | Remove common words ("the", "is", etc.) |
| `case_sensitive` | boolean | `false` | Preserve case in tokens |

**Response** (200 OK):
```json
{
  "namespace": "my_docs",
  "created": true
}
```

**Errors**:
- `400 Bad Request`: Invalid schema
- `409 Conflict`: Namespace already exists
- `500 Internal Server Error`: Storage error

---

### Upsert Documents

Insert or update documents in a namespace.

```http
POST /v1/namespaces/{namespace}/upsert
Content-Type: application/json
```

**Request Body**:
```json
{
  "documents": [
    {
      "id": 1,
      "vector": [0.1, 0.2, 0.3, ...],
      "attributes": {
        "title": "Rust Vector Database",
        "category": "tech",
        "score": 4.5,
        "published": true,
        "tags": ["rust", "database", "vectors"]
      }
    },
    {
      "id": 2,
      "vector": [0.2, 0.3, 0.4, ...],
      "attributes": {
        "title": "Full-Text Search with Tantivy",
        "category": "tech",
        "score": 4.8,
        "published": true,
        "tags": ["rust", "search"]
      }
    }
  ]
}
```

**Document Fields**:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | integer | Yes | Unique document ID (u64) |
| `vector` | array of floats | No | Embedding vector (must match `vector_dim`) |
| `attributes` | object | No | Map of attribute name to value |

**Notes**:
- If `id` already exists, document is updated (upsert semantics)
- `vector` can be omitted to update only attributes
- All attribute values must match types defined in schema
- Operation is atomic and durable (Write-Ahead Log)

**Response** (200 OK):
```json
{
  "upserted": 2
}
```

**Errors**:
- `400 Bad Request`: Invalid document format, type mismatch, or wrong vector dimension
- `404 Not Found`: Namespace does not exist
- `500 Internal Server Error`: Storage or index error

---

### Query Documents

Search for documents using vector similarity, full-text search, or both.

```http
POST /v1/namespaces/{namespace}/query
Content-Type: application/json
```

**Request Body**:
```json
{
  "vector": [0.1, 0.2, 0.3, ...],
  "full_text": {
    "fields": ["title", "description"],
    "query": "rust database",
    "weights": {
      "title": 2.0,
      "description": 1.0
    }
  },
  "filter": {
    "type": "and",
    "conditions": [
      {
        "field": "category",
        "op": "eq",
        "value": "tech"
      },
      {
        "field": "score",
        "op": "gte",
        "value": 4.0
      },
      {
        "field": "tags",
        "op": "contains_any",
        "value": ["rust", "go"]
      }
    ]
  },
  "top_k": 10,
  "include_vector": false,
  "include_attributes": ["title", "score"]
}
```

**Query Fields**:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `vector` | array | No | Query vector for similarity search |
| `full_text` | object | No | Full-text search configuration |
| `filter` | object | No | Attribute filter expression |
| `top_k` | integer | No | Number of results to return (default: 10, max: 1000) |
| `include_vector` | boolean | No | Include vectors in response (default: false) |
| `include_attributes` | array | No | Attributes to include (default: all) |

**FullTextQuery** (single field):
```json
{
  "field": "title",
  "query": "rust database"
}
```

**FullTextQuery** (multi-field):
```json
{
  "fields": ["title", "description"],
  "query": "rust database",
  "weights": {
    "title": 2.0,
    "description": 1.0
  }
}
```

**Fields**:
- `field`: Single field to search (mutually exclusive with `fields`)
- `fields`: Multiple fields to search
- `query`: Search query text
- `weights`: Optional map of field → weight multiplier (default: 1.0)

**FilterExpression**:

Supports two types: simple condition or complex expression.

**Simple Condition**:
```json
{
  "field": "category",
  "op": "eq",
  "value": "tech"
}
```

**Complex Expression** (AND/OR):
```json
{
  "type": "and",
  "conditions": [
    {"field": "published", "op": "eq", "value": true},
    {
      "type": "or",
      "conditions": [
        {"field": "category", "op": "eq", "value": "tech"},
        {"field": "category", "op": "eq", "value": "science"}
      ]
    }
  ]
}
```

**Filter Operators**:

| Operator | Types | Description | Example |
|----------|-------|-------------|---------|
| `eq` | All | Equals | `{"field": "category", "op": "eq", "value": "tech"}` |
| `ne` | All | Not equals | `{"field": "published", "op": "ne", "value": false}` |
| `gt` | Number | Greater than | `{"field": "score", "op": "gt", "value": 4.0}` |
| `gte` | Number | Greater than or equal | `{"field": "score", "op": "gte", "value": 4.0}` |
| `lt` | Number | Less than | `{"field": "score", "op": "lt", "value": 5.0}` |
| `lte` | Number | Less than or equal | `{"field": "score", "op": "lte", "value": 5.0}` |
| `contains` | Array | Array contains value | `{"field": "tags", "op": "contains", "value": "rust"}` |
| `contains_any` | Array | Array contains any value | `{"field": "tags", "op": "contains_any", "value": ["rust", "go"]}` |

**Response** (200 OK):
```json
{
  "results": [
    {
      "id": 1,
      "score": 0.95,
      "attributes": {
        "title": "Rust Vector Database",
        "score": 4.5
      }
    },
    {
      "id": 2,
      "score": 0.87,
      "attributes": {
        "title": "Full-Text Search with Tantivy",
        "score": 4.8
      }
    }
  ],
  "took_ms": 23
}
```

**Response Fields**:
- `results`: Array of matching documents
  - `id`: Document ID
  - `score`: Relevance score (0.0 - 1.0, higher is better)
  - `vector`: Embedding vector (if `include_vector` is true)
  - `attributes`: Document attributes (filtered by `include_attributes`)
- `took_ms`: Query execution time in milliseconds

**Query Modes**:

1. **Vector Search Only**:
   ```json
   { "vector": [...], "top_k": 10 }
   ```
   Uses RaBitQ for fast ANN search.

2. **Full-Text Search Only**:
   ```json
   { "full_text": { "field": "title", "query": "..." }, "top_k": 10 }
   ```
   Uses Tantivy BM25 algorithm.

3. **Hybrid Search** (Recommended):
   ```json
   { "vector": [...], "full_text": {...}, "top_k": 10 }
   ```
   Uses RRF (Reciprocal Rank Fusion) to merge results.

4. **Filtered Search**:
   ```json
   { "vector": [...], "filter": {...}, "top_k": 10 }
   ```
   Applies filter after vector/full-text search.

**Errors**:
- `400 Bad Request`: Invalid query format or wrong vector dimension
- `404 Not Found`: Namespace does not exist
- `500 Internal Server Error`: Search or storage error

---

## Data Types

### AttributeType

| Type | JSON Type | Description | Example |
|------|-----------|-------------|---------|
| `string` | string | UTF-8 text | `"hello"` |
| `int` | integer | 64-bit signed integer | `42` |
| `float` | number | 64-bit floating point | `3.14` |
| `bool` | boolean | True or false | `true` |
| `array_string` | array of strings | List of strings | `["a", "b"]` |

### VectorMetric

Distance/similarity metrics for vector search:

| Metric | Description | Range | Use Case |
|--------|-------------|-------|----------|
| `cosine` | Cosine similarity | -1 to 1 (1 = identical) | Normalized embeddings |
| `l2` | Euclidean distance | 0 to ∞ (0 = identical) | General purpose |
| `dot` | Dot product | -∞ to ∞ (higher = more similar) | Pre-normalized vectors |

**Recommendation**: Use `cosine` for most embedding models (e.g., OpenAI, Sentence Transformers).

### Supported Languages

Full-text search supports the following languages for stemming and stopwords:

| Language | Code | Stemming | Stopwords |
|----------|------|----------|-----------|
| Arabic | `arabic` | ✓ | ✓ |
| Danish | `danish` | ✓ | ✓ |
| Dutch | `dutch` | ✓ | ✓ |
| English | `english` | ✓ | ✓ |
| Finnish | `finnish` | ✓ | ✓ |
| French | `french` | ✓ | ✓ |
| German | `german` | ✓ | ✓ |
| Greek | `greek` | ✓ | ✓ |
| Hungarian | `hungarian` | ✓ | ✓ |
| Italian | `italian` | ✓ | ✓ |
| Norwegian | `norwegian` | ✓ | ✓ |
| Portuguese | `portuguese` | ✓ | ✓ |
| Romanian | `romanian` | ✓ | ✓ |
| Russian | `russian` | ✓ | ✓ |
| Spanish | `spanish` | ✓ | ✓ |
| Swedish | `swedish` | ✓ | ✓ |
| Tamil | `tamil` | ✓ | ✓ |
| Turkish | `turkish` | ✓ | ✓ |

## Examples

### Example 1: Vector Search for Similar Documents

Create a namespace and search for similar documents:

```bash
# Create namespace
curl -X PUT http://localhost:3000/v1/namespaces/papers \
  -H "Content-Type: application/json" \
  -d '{
    "schema": {
      "vector_dim": 384,
      "vector_metric": "cosine",
      "attributes": {
        "title": {"type": "string", "full_text": true},
        "year": {"type": "int", "indexed": true}
      }
    }
  }'

# Insert documents
curl -X POST http://localhost:3000/v1/namespaces/papers/upsert \
  -H "Content-Type: application/json" \
  -d '{
    "documents": [
      {
        "id": 1,
        "vector": [0.1, 0.2, ...],
        "attributes": {"title": "Attention is All You Need", "year": 2017}
      },
      {
        "id": 2,
        "vector": [0.15, 0.25, ...],
        "attributes": {"title": "BERT: Pre-training", "year": 2018}
      }
    ]
  }'

# Query similar papers
curl -X POST http://localhost:3000/v1/namespaces/papers/query \
  -H "Content-Type: application/json" \
  -d '{
    "vector": [0.12, 0.22, ...],
    "top_k": 5,
    "include_attributes": ["title", "year"]
  }'
```

### Example 2: Hybrid Search (Vector + Full-Text)

Search using both semantic similarity and keyword matching:

```bash
curl -X POST http://localhost:3000/v1/namespaces/papers/query \
  -H "Content-Type: application/json" \
  -d '{
    "vector": [0.1, 0.2, ...],
    "full_text": {
      "field": "title",
      "query": "transformer attention"
    },
    "top_k": 10
  }'
```

Response:
```json
{
  "results": [
    {
      "id": 1,
      "score": 0.92,
      "attributes": {
        "title": "Attention is All You Need",
        "year": 2017
      }
    }
  ],
  "took_ms": 45
}
```

### Example 3: Filtered Query

Search with attribute constraints:

```bash
curl -X POST http://localhost:3000/v1/namespaces/papers/query \
  -H "Content-Type: application/json" \
  -d '{
    "vector": [0.1, 0.2, ...],
    "filter": {
      "type": "and",
      "conditions": [
        {"field": "year", "op": "gte", "value": 2015},
        {"field": "year", "op": "lte", "value": 2020}
      ]
    },
    "top_k": 10
  }'
```

### Example 4: Multi-Field Full-Text Search

Search across multiple text fields with different weights:

```bash
# Create namespace with multiple text fields
curl -X PUT http://localhost:3000/v1/namespaces/articles \
  -H "Content-Type: application/json" \
  -d '{
    "schema": {
      "vector_dim": 768,
      "vector_metric": "cosine",
      "attributes": {
        "title": {
          "type": "string",
          "full_text": {
            "language": "english",
            "stemming": true,
            "remove_stopwords": true
          }
        },
        "abstract": {
          "type": "string",
          "full_text": true
        },
        "body": {
          "type": "string",
          "full_text": true
        }
      }
    }
  }'

# Multi-field search (title is 3x more important than body)
curl -X POST http://localhost:3000/v1/namespaces/articles/query \
  -H "Content-Type: application/json" \
  -d '{
    "full_text": {
      "fields": ["title", "abstract", "body"],
      "query": "machine learning neural networks",
      "weights": {
        "title": 3.0,
        "abstract": 2.0,
        "body": 1.0
      }
    },
    "top_k": 20
  }'
```

### Example 5: Complex Filter with Nested AND/OR

```bash
curl -X POST http://localhost:3000/v1/namespaces/papers/query \
  -H "Content-Type: application/json" \
  -d '{
    "vector": [0.1, 0.2, ...],
    "filter": {
      "type": "and",
      "conditions": [
        {"field": "year", "op": "gte", "value": 2015},
        {
          "type": "or",
          "conditions": [
            {"field": "category", "op": "eq", "value": "AI"},
            {"field": "category", "op": "eq", "value": "ML"}
          ]
        }
      ]
    },
    "top_k": 10
  }'
```

This query finds documents:
- Published in 2015 or later, AND
- Category is either "AI" or "ML"

## Rate Limits

Currently no rate limits enforced. Recommended client-side limits:
- Upserts: 10 requests/second
- Queries: 100 requests/second

## Errors

All errors return JSON with the following format:

```json
{
  "error": "Namespace not found: papers"
}
```

**HTTP Status Codes**:
- `200 OK`: Success
- `400 Bad Request`: Invalid request format or parameters
- `404 Not Found`: Namespace does not exist
- `409 Conflict`: Resource already exists
- `500 Internal Server Error`: Server error (storage, index, etc.)
- `503 Service Unavailable`: Server starting up or shutting down

## Client Libraries

Currently, Elacsym only provides an HTTP API. Client libraries are planned for:
- Python (P2)
- JavaScript/TypeScript (P2)
- Go (P2)

For now, use any HTTP client (e.g., `requests` in Python, `fetch` in JavaScript, `reqwest` in Rust).

## Authentication

**Current**: No authentication (not production-ready)

**Planned** (P2):
- API key authentication
- JWT tokens
- mTLS for node-to-node communication

**Workaround**: Deploy behind a reverse proxy (nginx, Envoy) with authentication.

## Versioning

API version is included in the URL path (`/v1/...`).

Breaking changes will increment the version number (e.g., `/v2/...`).

Current version: **v1**
