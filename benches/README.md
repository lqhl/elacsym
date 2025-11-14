# Performance Benchmarks

This directory contains performance benchmarks for elacsym.

## Running Benchmarks

```bash
# Run all benchmarks
cargo bench

# Run specific benchmark
cargo bench index_building

# Run with baseline comparison
cargo bench --bench vector_operations -- --save-baseline main
```

## Benchmarks

### Index Building
Measures the performance of building RaBitQ indexes with different dataset sizes:
- 100 vectors
- 500 vectors
- 1000 vectors
- 5000 vectors

### Query Performance
Measures query latency with different dataset sizes in the freshness layer:
- 100 vectors
- 500 vectors
- 1000 vectors

### Upsert Performance
Measures the performance of upserting batches of vectors.

## Results

Results are stored in `target/criterion/` and can be viewed with:

```bash
# Generate HTML reports
cargo install cargo-criterion
cargo criterion
```

Then open `target/criterion/report/index.html` in your browser.

## Comparing Baselines

```bash
# Save current performance as baseline
cargo bench -- --save-baseline current

# Make changes...

# Compare against baseline
cargo bench -- --baseline current
```
