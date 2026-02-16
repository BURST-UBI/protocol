//! BRN balance computation (client-side).

use burst_types::Timestamp;

/// Compute BRN balance for display.
///
/// `BRN = rate × (now − verified_at) − total_burned − total_staked`
pub fn compute_display_balance(
    verified_at: Timestamp,
    now: Timestamp,
    rate: u128,
    total_burned: u128,
    total_staked: u128,
) -> u128 {
    let elapsed = now.elapsed_since(verified_at) as u128;
    let accrued = rate * elapsed;
    accrued.saturating_sub(total_burned).saturating_sub(total_staked)
}
