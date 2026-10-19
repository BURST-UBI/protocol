//! Test-only accounting model for the component lifecycle story. This does not
//! perform admission, consensus, stake-receipt checking or atomic settlement.
//! Production settlement is tested in node/src/verification_commit.rs.

use burst_brn::BrnEngine;
use burst_types::{Timestamp, WalletAddress};

/// Grant a burn-backed TRST reward by creating a pending entry that the
/// recipient claims with a normal Receive block — so the reward flows through
/// the same on-chain validation as every other TRST (pending must exist and
/// amounts must match exactly).
///
/// The reward id is deterministic across nodes: every node derives the same
/// pending entry from the same verification outcome. The provenance anchors
/// the token at the reward id with the RECIPIENT as origin wallet — if the
/// recipient is later proven sybil, their reward TRST is revoked with them.
pub fn create_reward_pending<P: burst_store::pending::PendingStore>(
    pending_store: &P,
    recipient: &WalletAddress,
    context_wallet: &WalletAddress,
    kind: &'static [u8],
    amount: u128,
    ts: Timestamp,
) -> Result<burst_types::TxHash, String> {
    let reward_hash = burst_types::TxHash::new(burst_crypto::blake2b_256_multi(&[
        kind,
        context_wallet.as_str().as_bytes(),
        recipient.as_str().as_bytes(),
    ]));
    let info = burst_store::pending::PendingInfo {
        return_credit: false,
        source: context_wallet.clone(),
        amount,
        timestamp: ts,
        provenance: vec![burst_store::pending::PendingProvenance {
            basis: Default::default(),
            revoked_amounts: Default::default(),
            amount,
            origin: reward_hash,
            origin_wallet: recipient.clone(),
            origin_timestamp: ts,
            effective_origin_timestamp: ts,
        }],
    };
    pending_store
        .put_pending(recipient, &reward_hash, &info)
        .map_err(|e| e.to_string())?;
    Ok(reward_hash)
}

/// Resolve verifier stakes and rewards after a verification or challenge vote.
///
/// - Correct staked voters: stake unlocked, plus a TRST reward equal to their
///   share of the forfeited dissenter stakes (decision 33.7d). The reward is
///   burn-backed: the dissenters' BRN is burned below, and the minted TRST
///   sums to at most that burn — TRST is only ever created from burned BRN.
/// - Incorrect voters: stake forfeited (unlocked and burned).
pub fn resolve_verifier_outcomes<P: burst_store::pending::PendingStore>(
    brn_engine: &mut BrnEngine,
    pending_store: &P,
    verifiers: &[burst_verification::VerifierOutcome],
    context_wallet: &WalletAddress,
    ts: Timestamp,
) {
    for vo in verifiers {
        if vo.staked > 0 {
            if let Some(ws) = brn_engine.get_wallet_mut(&vo.address) {
                if vo.voted_correctly {
                    ws.total_staked = ws.total_staked.saturating_sub(vo.staked);
                    tracing::info!(
                        verifier = %vo.address,
                        staked = vo.staked,
                        "verifier stake returned (correct vote)"
                    );
                } else {
                    ws.total_staked = ws.total_staked.saturating_sub(vo.staked);
                    ws.total_burned = ws.total_burned.saturating_add(vo.staked);
                    tracing::info!(
                        verifier = %vo.address,
                        penalty = vo.penalty,
                        "dissenter verifier stake forfeited (burned)"
                    );
                }
            } else {
                tracing::warn!(
                    verifier = %vo.address,
                    "verifier wallet not tracked in BRN engine, cannot resolve stake"
                );
            }
        }
        if vo.voted_correctly && vo.trst_reward > 0 {
            match create_reward_pending(
                pending_store,
                &vo.address,
                context_wallet,
                b"verifier-reward",
                vo.trst_reward,
                ts,
            ) {
                Ok(reward_hash) => tracing::info!(
                    verifier = %vo.address,
                    reward = vo.trst_reward,
                    %reward_hash,
                    "verifier TRST reward granted as pending (funded by forfeited stakes)"
                ),
                Err(e) => tracing::error!(
                    verifier = %vo.address,
                    error = %e,
                    "failed to create verifier reward pending entry"
                ),
            }
        }
    }
}
