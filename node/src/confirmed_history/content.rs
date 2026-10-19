//! Authenticated proposal bodies carried with a closed account history.

use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ProposalWitness {
    pub block: BlockHash,
    pub bytes: Vec<u8>,
}

pub(crate) fn archive_key(hash: BlockHash) -> String {
    format!("checkpoint_governance_content/v1/{hash}")
}

pub(super) fn read(
    block: &StateBlock,
    used: &mut usize,
    get: impl FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>,
) -> Result<Option<ProposalWitness>, StoreError> {
    read_bounded(block, used, MAX_BYTES, get)
}

pub(crate) fn read_bounded(
    block: &StateBlock,
    used: &mut usize,
    limit: usize,
    mut get: impl FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>,
) -> Result<Option<ProposalWitness>, StoreError> {
    if !crate::governance_content::is_full(block) {
        return Ok(None);
    }
    let manifest =
        crate::governance_content::authenticate(block).map_err(|error| invalid(&error))?;
    *used = used
        .checked_add(manifest.length as usize)
        .filter(|size| *size <= limit)
        .ok_or_else(|| invalid("history proposal bodies exceed replay byte limit"))?;
    let bytes = match get(&crate::governance_content::key(block.hash))? {
        Some(bytes) => bytes,
        None => get(&archive_key(block.hash))?
            .ok_or_else(|| invalid("history proposal body is missing"))?,
    };
    manifest
        .decode_verified(&bytes)
        .map_err(|error| invalid(&error.to_string()))?;
    Ok(Some(ProposalWitness {
        block: block.hash,
        bytes,
    }))
}

pub(super) fn validate(
    blocks: &[StateBlock],
    contents: &mut [ProposalWitness],
) -> Result<usize, StoreError> {
    let required: HashMap<_, _> = blocks
        .iter()
        .filter(|block| crate::governance_content::is_full(block))
        .map(|block| (block.hash, block))
        .collect();
    if required.len() != contents.len() {
        return Err(invalid(
            "history proposal body count differs from signed blocks",
        ));
    }
    contents.sort_by_key(|witness| *witness.block.as_bytes());
    let mut previous = None;
    let mut used = 0usize;
    for witness in contents {
        if previous == Some(witness.block) {
            return Err(invalid("duplicate history proposal body"));
        }
        previous = Some(witness.block);
        used = used
            .checked_add(witness.bytes.len())
            .filter(|size| *size <= MAX_BYTES)
            .ok_or_else(|| invalid("history proposal bodies exceed replay byte limit"))?;
        let block = required
            .get(&witness.block)
            .ok_or_else(|| invalid("unbound history proposal body"))?;
        crate::governance_content::authenticate(block)
            .map_err(|error| invalid(&error))?
            .decode_verified(&witness.bytes)
            .map_err(|error| invalid(&error.to_string()))?;
    }
    Ok(used)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_content_budget_is_checked_before_loading_the_next_body() {
        let mut block =
            crate::genesis_state::block(NetworkId::Dev, &burst_types::ProtocolParams::default());
        let encoded = burst_governance::content::EncodedContent::new(
            &burst_governance::ProposalContent::ConstitutionalDiff {
                title: "Limit".into(),
                operations: vec![burst_governance::proposal::AmendmentOp::RepealArticle {
                    article_number: 1,
                }],
            },
        )
        .unwrap();
        block.previous = block.hash;
        block.payment_epoch = block.hash;
        block.block_type = burst_ledger::BlockType::GovernanceProposal;
        (block.link, block.origin, block.transaction) = encoded.manifest().block_fields().unwrap();
        block.hash = block.compute_hash();
        block.signature = burst_crypto::sign_message(
            block.hash.as_bytes(),
            &burst_crypto::keypair_from_seed(&[0; 32]).private,
        );
        let size = encoded.bytes().len();
        let mut used = MAX_BYTES - size + 1;
        assert!(read(&block, &mut used, |_| panic!(
            "must refuse before loading the body"
        ))
        .is_err());
        let mut used = MAX_BYTES - size;
        let witness = read(&block, &mut used, |_| Ok(Some(encoded.bytes().to_vec())))
            .unwrap()
            .unwrap();
        assert_eq!(used, MAX_BYTES);
        assert_eq!(witness.bytes, encoded.bytes());
    }
}
