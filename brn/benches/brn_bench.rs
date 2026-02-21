use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

use burst_brn::{BrnEngine, BrnWalletState, RateHistory};
use burst_types::Timestamp;

fn make_rate_history_with_segments(n: usize) -> RateHistory {
    let mut history = RateHistory::new(100, Timestamp::new(0));
    for i in 1..n {
        let change_at = Timestamp::new(i as u64 * 1000);
        history
            .apply_rate_change(100 + i as u128, change_at)
            .unwrap();
    }
    history
}

fn bench_brn_balance_computation(c: &mut Criterion) {
    let mut group = c.benchmark_group("brn_balance");
    let state = BrnWalletState::new(Timestamp::new(0));

    for segment_count in [1, 10, 100, 1000] {
        let history = make_rate_history_with_segments(segment_count);
        let now = Timestamp::new(segment_count as u64 * 1000 + 500);

        group.bench_with_input(
            BenchmarkId::new("available_balance", segment_count),
            &segment_count,
            |b, _| {
                b.iter(|| black_box(state.available_balance(black_box(&history), black_box(now))));
            },
        );
    }

    group.finish();
}

fn bench_brn_balance_checked(c: &mut Criterion) {
    let mut group = c.benchmark_group("brn_balance_checked");
    let state = BrnWalletState::new(Timestamp::new(0));

    for segment_count in [1, 10, 100, 1000] {
        let history = make_rate_history_with_segments(segment_count);
        let now = Timestamp::new(segment_count as u64 * 1000 + 500);

        group.bench_with_input(
            BenchmarkId::new("available_balance_checked", segment_count),
            &segment_count,
            |b, _| {
                b.iter(|| {
                    black_box(state.available_balance_checked(black_box(&history), black_box(now)))
                });
            },
        );
    }

    group.finish();
}

fn bench_brn_total_accrued(c: &mut Criterion) {
    let mut group = c.benchmark_group("brn_total_accrued");
    let state = BrnWalletState::new(Timestamp::new(0));

    for segment_count in [1, 10, 100, 1000] {
        let history = make_rate_history_with_segments(segment_count);
        let now = Timestamp::new(segment_count as u64 * 1000 + 500);

        group.bench_with_input(
            BenchmarkId::new("total_accrued", segment_count),
            &segment_count,
            |b, _| {
                b.iter(|| {
                    black_box(history.total_accrued(black_box(state.verified_at), black_box(now)))
                });
            },
        );
    }

    group.finish();
}

fn bench_brn_engine_burn(c: &mut Criterion) {
    let engine = BrnEngine::with_rate(1_000_000, Timestamp::new(0));

    c.bench_function("engine_record_burn", |b| {
        b.iter_batched(
            || {
                let state = BrnWalletState::new(Timestamp::new(0));
                (state, Timestamp::new(10_000))
            },
            |(mut state, now)| {
                let _ = black_box(engine.record_burn(&mut state, 100, now));
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

fn bench_brn_rate_change(c: &mut Criterion) {
    c.bench_function("engine_apply_rate_change", |b| {
        b.iter_batched(
            || BrnEngine::with_rate(100, Timestamp::new(0)),
            |mut engine| {
                for i in 1u64..=10 {
                    engine
                        .apply_rate_change(black_box(100 + i as u128), Timestamp::new(i * 1000))
                        .unwrap();
                }
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

criterion_group!(
    benches,
    bench_brn_balance_computation,
    bench_brn_balance_checked,
    bench_brn_total_accrued,
    bench_brn_engine_burn,
    bench_brn_rate_change,
);
criterion_main!(benches);
