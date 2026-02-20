#![no_main]

use libfuzzer_sys::fuzz_target;

use burst_brn::state::RateHistory;
use burst_types::Timestamp;

// Fuzz BRN balance computation with arbitrary rate segments and timestamps.
// Ensures the computation never panics regardless of input.
fuzz_target!(|data: &[u8]| {
    if data.len() < 16 {
        return;
    }

    let initial_rate = u64::from_le_bytes([
        data[0], data[1], data[2], data[3],
        data[4], data[5], data[6], data[7],
    ]) as u128;

    let query_time = u64::from_le_bytes([
        data[8], data[9], data[10], data[11],
        data[12], data[13], data[14], data[15],
    ]);

    let mut rate_history = RateHistory::new(initial_rate, Timestamp::new(0));

    let remaining = &data[16..];
    let mut offset = 0;
    let mut last_time = 0u64;
    while offset + 16 <= remaining.len() {
        let rate = u64::from_le_bytes([
            remaining[offset], remaining[offset + 1],
            remaining[offset + 2], remaining[offset + 3],
            remaining[offset + 4], remaining[offset + 5],
            remaining[offset + 6], remaining[offset + 7],
        ]) as u128;

        let time_offset = u64::from_le_bytes([
            remaining[offset + 8], remaining[offset + 9],
            remaining[offset + 10], remaining[offset + 11],
            remaining[offset + 12], remaining[offset + 13],
            remaining[offset + 14], remaining[offset + 15],
        ]);

        last_time = last_time.saturating_add(time_offset.min(100_000));
        let _ = rate_history.apply_rate_change(rate, Timestamp::new(last_time));

        offset += 16;
    }

    let verified_at = Timestamp::new(0);
    let now = Timestamp::new(query_time);

    // These must never panic
    let _ = rate_history.total_accrued(verified_at, now);
    let _ = rate_history.total_accrued_checked(verified_at, now);
});
