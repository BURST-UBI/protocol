use super::*;

fn activation_roots(
    activation: &crate::verification_activation::Activation,
    visit: &mut impl FnMut(Reference<'_>) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    visit(Reference::Account(&activation.target))?;
    visit(Reference::CommitteeContext(activation.context))?;
    visit(Reference::Checkpoint(activation.selection))?;
    visit(Reference::Parameters(activation.params_hash))?;
    visit(Reference::Boundary(Timestamp::new(activation.started_at)))?;
    visit(Reference::Boundary(Timestamp::new(activation.deadline)))
}

pub(super) fn visit(
    key: &str,
    bytes: &[u8],
    mut visit: impl FnMut(Reference<'_>) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    if key.starts_with("verification_activation/v1/") {
        let activation = crate::verification_activation::decode(bytes).map_err(invalid)?;
        if key != crate::verification_activation::key(activation.context) {
            return Err(invalid("activation key mismatch"));
        }
        return activation_roots(&activation, &mut visit);
    }
    if key.starts_with("verification_deadline/v1/") {
        let completion = crate::verification_deadline::decode(bytes).map_err(invalid)?;
        if key != crate::verification_deadline::key(completion.activation.context) {
            return Err(invalid("deadline key mismatch"));
        }
        activation_roots(&completion.activation, &mut visit)?;
        // Replay selects a signed threshold endorsement or challenge admission.
        visit(Reference::Block(completion.lifecycle))?;
        visit(Reference::Boundary(Timestamp::new(completion.completed_at)))?;
        for voter in &completion.absent { visit(Reference::Account(voter))?; }
        return Ok(());
    }
    if let Some(suffix) = key.strip_prefix("verification_default/v1/") {
        let (target, voter) = suffix.split_once('/').ok_or_else(|| invalid("invalid default key"))?;
        if [target, voter].iter().any(|account| !account.starts_with("brst_") || account.len() <= 5 || account.contains('/')) {
            return Err(invalid("invalid default account"));
        }
        let context = dependencies::decode_source(bytes)?;
        visit(Reference::Account(&WalletAddress::new(target)))?;
        visit(Reference::Account(&WalletAddress::new(voter)))?;
        return visit(Reference::CommitteeContext(context));
    }
    Err(invalid("unsupported deadline retention schema"))
}
