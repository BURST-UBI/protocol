use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn ed25519_sign_bench(c: &mut Criterion) {
    let kp = burst_crypto::generate_keypair();
    let msg = [42u8; 128];

    c.bench_function("ed25519_sign_128B", |b| {
        b.iter(|| burst_crypto::sign_message(black_box(&msg), &kp.private))
    });
}

fn ed25519_verify_bench(c: &mut Criterion) {
    let kp = burst_crypto::generate_keypair();
    let msg = [42u8; 128];
    let sig = burst_crypto::sign_message(&msg, &kp.private);

    c.bench_function("ed25519_verify_128B", |b| {
        b.iter(|| burst_crypto::verify_signature(black_box(&msg), &sig, &kp.public))
    });
}

fn blake2b_256_bench(c: &mut Criterion) {
    let data = [0xABu8; 256];

    c.bench_function("blake2b_256_256B", |b| {
        b.iter(|| burst_crypto::blake2b_256(black_box(&data)))
    });
}

fn blake2b_256_1kb_bench(c: &mut Criterion) {
    let data = vec![0xCDu8; 1024];

    c.bench_function("blake2b_256_1KB", |b| {
        b.iter(|| burst_crypto::blake2b_256(black_box(&data)))
    });
}

fn blake2b_multi_bench(c: &mut Criterion) {
    let parts: Vec<&[u8]> = vec![&[1u8; 32], &[2u8; 64], &[3u8; 128]];

    c.bench_function("blake2b_256_multi_3parts", |b| {
        b.iter(|| burst_crypto::blake2b_256_multi(black_box(&parts)))
    });
}

fn hash_block_bench(c: &mut Criterion) {
    let block_bytes = vec![0xFFu8; 512];

    c.bench_function("hash_block_512B", |b| {
        b.iter(|| burst_crypto::hash_block(black_box(&block_bytes)))
    });
}

fn keypair_generation_bench(c: &mut Criterion) {
    c.bench_function("keypair_generate", |b| {
        b.iter(|| burst_crypto::generate_keypair())
    });
}

criterion_group!(
    benches,
    ed25519_sign_bench,
    ed25519_verify_bench,
    blake2b_256_bench,
    blake2b_256_1kb_bench,
    blake2b_multi_bench,
    hash_block_bench,
    keypair_generation_bench,
);
criterion_main!(benches);
