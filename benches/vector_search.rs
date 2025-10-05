use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn vector_search_benchmark(c: &mut Criterion) {
    c.bench_function("vector_search_1k", |b| {
        b.iter(|| {
            // TODO: Implement benchmark
            black_box(42)
        })
    });
}

criterion_group!(benches, vector_search_benchmark);
criterion_main!(benches);
