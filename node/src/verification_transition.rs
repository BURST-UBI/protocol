//! Verification effects shared by ordinary admission and branch replacement.
use burst_ledger::{BlockType, StateBlock};
use burst_store_lmdb::LmdbWriteTransaction;
use burst_types::{NetworkId, ProtocolParams};
use burst_verification::VerificationOrchestrator;

pub(crate) fn supports(kind: &BlockType) -> bool {
    matches!(
        kind,
        BlockType::Endorse | BlockType::Challenge | BlockType::VerificationVote
    )
}

/// Apply lifecycle effects after economic staging. The caller owns persistence
/// and commit, and must discard candidate engines and abort on error.
pub(crate) fn apply_staged(
    batch: &mut LmdbWriteTransaction<'_>,
    block: &StateBlock,
    effect: &crate::block_economics::EconomicResult,
    params: &ProtocolParams,
    network: NetworkId,
    verification: &mut VerificationOrchestrator,
    engines: &mut impl crate::economic_candidate::Engines,
) -> Result<(), String> {
    match block.block_type {
        BlockType::Endorse => {
            let crate::block_economics::EconomicResult::Endorse { burn_amount, .. } = effect else {
                return Err("endorsement economic result missing".into());
            };
            crate::endorsement_commit::apply_staged(
                batch,
                block,
                *burn_amount,
                network,
                params,
                verification,
                engines,
            )
        }
        BlockType::Challenge | BlockType::VerificationVote => {
            crate::verification_commit::collect_staged(
                batch,
                block,
                effect,
                params,
                verification,
                engines,
                network,
            )
        }
        _ => Err("unsupported verification transition".into()),
    }
}
