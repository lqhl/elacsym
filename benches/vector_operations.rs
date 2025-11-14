//! Benchmarks for vector operations

use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};
use elacsym_builder::{BuilderConfig, BuilderService};
use elacsym_core::{DistanceMetric, Metadata, Vector, VectorId};
use elacsym_index::{FreshnessLayer, IndexBuilder, IndexConfig, SearchConfig};
use elacsym_metadata::{MetadataService, NamespaceMetadata, RocksDBStore};
use elacsym_storage::{BlobStorage, BlobStorageConfig, StorageBackend};
use std::sync::Arc;

fn generate_vectors(count: usize, dimension: usize) -> Vec<Vector> {
    (0..count)
        .map(|i| {
            let values: Vec<f32> = (0..dimension).map(|j| (i * dimension + j) as f32 / 1000.0).collect();
            Vector::new(
                VectorId::from(format!("vec_{}", i)),
                values,
                Metadata::new(),
                "bench",
            )
        })
        .collect()
}

fn bench_index_building(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_building");

    for size in [100, 500, 1000, 5000].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            size,
            |b, &size| {
                let vectors = generate_vectors(size, 128);
                let config = IndexConfig::new(128, DistanceMetric::L2);
                let builder = IndexBuilder::new(config).unwrap();

                b.iter(|| {
                    let _slab = builder.build("bench".to_string(), 0, black_box(&vectors)).unwrap();
                });
            },
        );
    }

    group.finish();
}

fn bench_query_performance(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("query_performance");

    for size in [100, 500, 1000].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            size,
            |b, &size| {
                // Setup
                let vectors = generate_vectors(size, 128);
                let config = IndexConfig::new(128, DistanceMetric::L2);
                let layer = FreshnessLayer::new("bench".to_string(), config, 10000);

                runtime.block_on(async {
                    layer.add_batch(vectors).await.unwrap();
                });

                let query = vec![0.5; 128];
                let search_config = SearchConfig::new(10, 16);

                b.to_async(&runtime).iter(|| async {
                    let _results = layer.search(black_box(&query), &search_config).await.unwrap();
                });
            },
        );
    }

    group.finish();
}

fn bench_upsert_performance(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("upsert_performance");

    group.bench_function("upsert_100", |b| {
        let vectors = generate_vectors(100, 128);

        b.to_async(&runtime).iter(|| async {
            // Setup storage and metadata
            let storage_config = BlobStorageConfig {
                base_path: "bench_slabs".to_string(),
                backend: StorageBackend::Memory,
            };
            let storage = Arc::new(BlobStorage::new(storage_config).await.unwrap());

            let temp_dir = tempfile::tempdir().unwrap();
            let metadata_store = Arc::new(RocksDBStore::new(temp_dir.path()).unwrap());
            let metadata = Arc::new(MetadataService::new(metadata_store));

            let ns_metadata = NamespaceMetadata {
                name: "bench".to_string(),
                dimension: 128,
                vector_count: 0,
                partition_count: 0,
            };
            metadata.upsert_namespace(ns_metadata).await.unwrap();

            let builder_config = BuilderConfig {
                min_vectors: 10000,
                max_vectors_per_slab: 100_000,
                build_interval_secs: 3600,
                index_config: IndexConfig::new(128, DistanceMetric::L2),
            };

            let builder = Arc::new(BuilderService::new(
                builder_config,
                Arc::clone(&storage),
                Arc::clone(&metadata),
            ));

            // Benchmark
            builder.add_vectors("bench", black_box(vectors.clone())).await.unwrap();
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_index_building,
    bench_query_performance,
    bench_upsert_performance
);
criterion_main!(benches);
