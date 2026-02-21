use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

use burst_types::BlockHash;
use burst_work::{validate_work, WorkGenerator};

fn bench_pow_generation(c: &mut Criterion) {
    let mut group = c.benchmark_group("pow_generation");
    let generator = WorkGenerator;
    let block_hash = BlockHash::new([0x42; 32]);

    // Low difficulty levels that complete quickly enough for benchmarking.
    // Higher difficulty => more iterations to find a valid nonce.
    for difficulty in [0u64, 1_000, 10_000, 100_000] {
        group.bench_with_input(
            BenchmarkId::new("generate", difficulty),
            &difficulty,
            |b, &diff| {
                b.iter(|| {
                    black_box(
                        generator
                            .generate(black_box(&block_hash), black_box(diff))
                            .unwrap(),
                    )
                });
            },
        );
    }

    group.finish();
}

fn bench_pow_validation(c: &mut Criterion) {
    let generator = WorkGenerator;
    let block_hash = BlockHash::new([0x42; 32]);
    let difficulty = 10_000u64;
    let nonce = generator.generate(&block_hash, difficulty).unwrap();

    c.bench_function("pow_validate_valid", |b| {
        b.iter(|| {
            black_box(validate_work(
                black_box(&block_hash),
                black_box(nonce.0),
                black_box(difficulty),
            ))
        });
    });

    c.bench_function("pow_validate_invalid", |b| {
        let bad_hash = BlockHash::new([0xFF; 32]);
        b.iter(|| {
            black_box(validate_work(
                black_box(&bad_hash),
                black_box(nonce.0),
                black_box(u64::MAX),
            ))
        });
    });
}

fn bench_pow_validation_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("pow_validation_throughput");
    let generator = WorkGenerator;

    let hashes_and_nonces: Vec<_> = (0u8..10)
        .map(|i| {
            let hash = BlockHash::new([i; 32]);
            let nonce = generator.generate(&hash, 1_000).unwrap();
            (hash, nonce)
        })
        .collect();

    group.bench_function("validate_10_blocks", |b| {
        b.iter(|| {
            for (hash, nonce) in &hashes_and_nonces {
                black_box(validate_work(hash, nonce.0, 1_000));
            }
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_pow_generation,
    bench_pow_validation,
    bench_pow_validation_throughput,
);
criterion_main!(benches);
