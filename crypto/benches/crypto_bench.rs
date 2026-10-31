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
        b.iter(burst_crypto::generate_keypair)
    });
}

fn reference_blake2b_256(data: &[u8]) -> [u8; 32] {
    use blake2::{Blake2b, Digest, digest::consts::U32};
    Blake2b::<U32>::digest(data).into()
}

// Compare the SIMD backend with RustCrypto across record and page sizes.
fn blake2b_backend_comparison(c: &mut Criterion) {
    use criterion::{BenchmarkId, Throughput};
    let mut group = c.benchmark_group("blake2b_backend");
    for length in [0usize, 64, 84, 102, 127, 128, 129, 256, 1024, 4096, 16384] {
        let data: Vec<u8> = (0..length).map(|i| (i % 251) as u8).collect();
        let mut params = blake2b_simd::Params::new();
        params.hash_length(32);
        assert_eq!(params.hash(&data).as_bytes(), reference_blake2b_256(&data));
        for chunk in [1, 31, 128, 129, 512] {
            let mut streamed = params.to_state();
            streamed.update(&[]);
            for part in data.chunks(chunk) { streamed.update(part); }
            streamed.update(&[]);
            assert_eq!(streamed.finalize().as_bytes(), reference_blake2b_256(&data));
        }
        group.throughput(Throughput::Bytes(length as u64));
        group.bench_with_input(BenchmarkId::new("rustcrypto_reference", length), &data, |b, data| {
            b.iter(|| reference_blake2b_256(black_box(data)))
        });
        group.bench_with_input(BenchmarkId::new("simd_candidate", length), &data, |b, data| {
            b.iter(|| blake2b_simd::Params::new().hash_length(32).hash(black_box(data)))
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    ed25519_sign_bench,
    ed25519_verify_bench,
    blake2b_256_bench,
    blake2b_256_1kb_bench,
    blake2b_multi_bench,
    hash_block_bench,
    blake2b_backend_comparison,
    keypair_generation_bench,
);
criterion_main!(benches);
