use super::*;
use crate::confirmed_replay::{snapshot::SelectionSource, ConfirmedReplay};
use burst_verification::{OrchestratorSnapshot, VerificationEvent};

/// A local snapshot may name a published committee before certified activation.
/// Its context is only a hint: derive the committee again, then compare all fields.
pub(in crate::confirmed_replay) fn materialize(
    actual: OrchestratorSnapshot,
    state: &mut ConfirmedReplay,
    selections: &(impl SelectionSource + ?Sized),
    directory: &std::path::Path,
) -> Result<(), StoreError> {
    let mut expected = state.verification.clone();
    for selection in selections.iter_selections() {
        let selection = selection.map_err(invalid)?;
        let request = selection.replayed().request();
        let target = request.target();
        let context = request
            .vote_context(&selection.certificate().checkpoint)
            .map_err(invalid)?;
        if actual.selection_context(target) != Some(context)
            || expected.selection_context(target) == Some(context)
        {
            continue;
        }
        if expected.selection_request(target) != Some(request) {
            return Err(invalid("published committee differs from replayed request"));
        }
        if !expected.drain_events().is_empty() {
            return Err(invalid("committee recovery has uncommitted replay events"));
        }
        let beacon = selection
            .beacon()
            .ok_or_else(|| invalid("published committee has no certified beacon"))?;
        expected
            .install_selection(target, selection.parent(), selection.certificate(), beacon)
            .map_err(invalid)?;
        let events = expected.drain_events();
        if events.len() != 1
            || !matches!(&events[0], VerificationEvent::VerifiersSelected { wallet, .. } if wallet == target)
        {
            return Err(invalid("unexpected recovered committee effects"));
        }
    }
    if !super::super::canonical::equivalent_verification(
        actual.snapshot_view(),
        expected.snapshot_view(),
        directory,
    )
    .map_err(invalid)?
    {
        return Err(invalid(
            "published verification state differs from certified replay",
        ));
    }
    state.verification = expected;
    Ok(())
}
