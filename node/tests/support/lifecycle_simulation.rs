//! Test-only accounting model for the component lifecycle story. This does not
//! perform admission, consensus, stake-receipt checking or atomic settlement.
//! Production settlement is tested in node/src/verification_commit.rs.

use burst_brn::BrnEngine;
use burst_types::{Timestamp, WalletAddress};

/// Insert a fixture reward pending receipt, with the recipient as its originator.
/// The ID hashes only the kind, context wallet and recipient; callers must avoid
/// reusing that tuple for distinct outcomes. This helper checks neither burn
/// backing nor consensus. The fixture exercises receipt validation separately.
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

/// Apply supplied outcomes directly to the component fixture's balances.
/// Matching voters get stake refunds and any caller-supplied reward; dissenters'
/// stakes are burned. This helper does not verify the reward total or settle
/// atomically, and missing wallets and storage failures are only logged.
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
