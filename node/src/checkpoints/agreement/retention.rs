use super::*;

/// A complete local round and its replay inputs from an observed storage row.
/// Decoding checks integrity/coverage, not protocol authority. Only a checked
/// recovery cut establishes the signer/parent binding; manifests still require
/// semantic replay before they can authorize any replacement or signature.
pub(crate) struct RoundJournal<'a> {
    #[cfg(test)]
    pub key: &'a str,
    #[cfg(test)]
    pub representative: &'a str,
    #[cfg(test)]
    pub height: u64,
    #[cfg(test)]
    pub state: &'a RoundState,
    pub manifests: Option<&'a [ReplayManifest]>,
}

pub(crate) fn visit_round(
    key: &str,
    bytes: &[u8],
    network: burst_types::NetworkId,
    genesis: burst_types::BlockHash,
    visit: impl FnOnce(RoundJournal<'_>) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let prefix = format!("checkpoint_round/v1/{}/{genesis}/", network.as_str());
    let (representative, height) = key
        .strip_prefix(&prefix)
        .and_then(|suffix| suffix.split_once('/'))
        .ok_or_else(|| invalid("unsupported retained round namespace"))?;
    if burst_crypto::decode_address(representative).is_none() {
        return Err(invalid("invalid retained round signer"));
    }
    let number: u64 = height.parse().map_err(invalid)?;
    if number == 0 || height != number.to_string() {
        return Err(invalid("invalid retained round height"));
    }
    let snapshot = decode_snapshot(bytes)?;
    snapshot.validate_manifest_coverage()?;
    if snapshot.manifests.is_none() && !snapshot.state.retained_values().is_empty() {
        return Err(invalid(
            "retained round choices require replay manifest reconstruction",
        ));
    }
    visit(RoundJournal {
        #[cfg(test)]
        key,
        #[cfg(test)]
        representative,
        #[cfg(test)]
        height: number,
        #[cfg(test)]
        state: &snapshot.state,
        manifests: snapshot.manifests.as_deref(),
    })
}

#[cfg(test)]
pub(crate) fn empty_round_fixture(
    anchor: &CheckpointAnchor,
    representative: &WalletAddress,
) -> (String, Vec<u8>) {
    let mut snapshot = Snapshot {
        version: 3,
        state: RoundState::new(anchor, representative.clone()).unwrap(),
        manifests: Some(Vec::new()),
        checksum: [0; 32],
    };
    snapshot.checksum = snapshot.checksum().unwrap();
    (
        state_key(anchor, representative).unwrap(),
        bincode::serialize(&snapshot).unwrap(),
    )
}
