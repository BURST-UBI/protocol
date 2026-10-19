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

pub(crate) fn apply(
    batch: &mut LmdbWriteTransaction<'_>,
    block: &StateBlock,
    effect: &crate::ledger_bridge::EconomicResult,
    params: &ProtocolParams,
    network: NetworkId,
    verification: &mut VerificationOrchestrator,
    engines: (&mut burst_brn::BrnEngine, &mut burst_trst::TrstEngine),
) -> Result<(), String> {
    match block.block_type {
        BlockType::Endorse => {
            let crate::ledger_bridge::EconomicResult::Endorse { burn_amount, .. } = effect else {
                return Err("endorsement economic result missing".into());
            };
            crate::endorsement_commit::apply(
                batch,
                block,
                *burn_amount,
                network,
                params,
                verification,
                engines,
            )
        }
        BlockType::Challenge | BlockType::VerificationVote => crate::verification_commit::collect(
            batch,
            block,
            effect,
            params,
            verification,
            engines,
            network,
        ),
        _ => Err("unsupported verification transition".into()),
    }
}
