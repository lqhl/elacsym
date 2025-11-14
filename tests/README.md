# Integration Tests

This directory contains integration tests for elacsym.

## Running Tests

```bash
# Run all tests
cargo test

# Run specific test
cargo test test_upsert_and_query

# Run with output
cargo test -- --nocapture
```

## Test Coverage

- `test_upsert_and_query`: Tests vector insertion and querying through the freshness layer
- `test_namespace_operations`: Tests namespace creation and retrieval
- `test_vector_operations`: Tests basic vector operations
