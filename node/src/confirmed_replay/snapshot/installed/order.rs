use super::*;
use burst_ledger::{BlockType, StateBlock};
use burst_types::BlockHash;
use std::collections::{BTreeSet, HashSet};

// Local links choose an order, not state or authority. Replay still authenticates
// and executes every block, then compares all resulting economics and metadata.
pub(super) fn read(
    batch: &LmdbWriteTransaction<'_>,
    applied: &super::super::tail::cemented::AppliedTail,
    base: &super::super::SnapshotContinuation,
) -> Result<Vec<(BlockHash, BlockHash)>, StoreError> {
        read_view(&batch.read_view(), applied, base)
    }

    pub(super) fn read_view(
    batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    applied: &super::super::tail::cemented::AppliedTail,
    base: &super::super::SnapshotContinuation,
) -> Result<Vec<(BlockHash, BlockHash)>, StoreError> {
    let tail = applied.tail();
    let fresh: HashSet<_> = tail.block_hashes().collect();
    let mut edges = BTreeSet::new();
    for block in tail.blocks() {
        let block = block?;
        if block.block_type == BlockType::VerificationVote
            && batch
                .get_meta(&format!(
                    "verification_transition_sources/v1/{}",
                    block.hash
                ))?
                .is_some()
        {
            // Settlement ran when this vote arrived, after the other panel
            // votes. Reproduce that local order without trusting its outcome:
            // dependency reads check the committed journal, and replay must
            // derive the same complete lifecycle and economic state.
            for source in crate::verification_commit::dependencies::read(batch, &block)? {
                if fresh.contains(&source) {
                    edges.insert((block.hash, source));
                }
            }
        }
        if let Some(bytes) =
            batch.get_meta(&format!("verification_identity_binding/v1/{}", block.hash))?
        {
            let authenticated_source = base.state().lifecycle_metadata.get(&format!(
                "verification_identity_source/v1/{}",
                block.account
            ));
            binding_edges(
                &mut edges,
                &fresh,
                &block,
                &bytes,
                authenticated_source.map(Vec::as_slice),
            )?;
        }
        if matches!(
            block.block_type,
            BlockType::Endorse | BlockType::VerificationVote
        ) {
            let target = crate::block_economics::wallet_from_link(&block.link)
                .ok_or_else(|| invalid("invalid local identity ordering target"))?;
            let source = batch.get_meta(&format!("verification_identity_source/v1/{target}"))?;
            if source.as_deref() == Some(block.hash.as_bytes().as_slice())
                && batch.get_meta(&format!("verification_identity_bound/v1/{target}"))? != source
            {
                let head = batch.get_account(&target)?.head;
                if fresh.contains(&head) {
                    edges.insert((block.hash, head));
                }
            }
        }
    }
    Ok(edges.into_iter().collect())
}

fn binding_edges(
    edges: &mut BTreeSet<(BlockHash, BlockHash)>,
    fresh: &HashSet<BlockHash>,
    block: &StateBlock,
    bytes: &[u8],
    authenticated_source: Option<&[u8]>,
) -> Result<(), StoreError> {
    let source = BlockHash::new(bytes.try_into().map_err(invalid)?);
    if source.is_zero() {
        return Err(invalid("zero local identity ordering source"));
    }
    if !fresh.contains(&source) {
        // This transition has already executed in the authenticated base. Its
        // original block body is not needed to order the new applied suffix.
        if authenticated_source != Some(bytes) {
            return Err(invalid(
                "identity ordering source is absent from authenticated state",
            ));
        }
        return Ok(());
    }
    edges.insert((block.hash, source));
    if fresh.contains(&block.previous) {
        edges.insert((source, block.previous));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historical_binding_requires_the_exact_authenticated_wallet_source() {
        let block = crate::genesis_state::block(NetworkId::Dev, &ProtocolParams::default());
        let source = BlockHash::new([71; 32]);
        let other = BlockHash::new([72; 32]);
        let mut edges = BTreeSet::new();
        let fresh = HashSet::from([block.hash]);
        binding_edges(
            &mut edges,
            &fresh,
            &block,
            source.as_bytes(),
            Some(source.as_bytes()),
        )
        .unwrap();
        assert!(edges.is_empty());
        for authenticated in [None, Some(other.as_bytes().as_slice())] {
            assert!(
                binding_edges(&mut edges, &fresh, &block, source.as_bytes(), authenticated)
                    .is_err()
            );
            assert!(edges.is_empty());
        }
        for malformed in [vec![], vec![0; 32], vec![71; 31], vec![71; 33]] {
            assert!(
                binding_edges(&mut edges, &fresh, &block, &malformed, Some(&malformed)).is_err()
            );
        }
    }

    #[test]
    fn fresh_identity_source_is_bracketed_without_reordering_old_predecessors() {
        let mut block = crate::genesis_state::block(NetworkId::Dev, &ProtocolParams::default());
        block.previous = BlockHash::new([70; 32]);
        let source = BlockHash::new([71; 32]);
        for previous_is_fresh in [false, true] {
            let mut fresh = HashSet::from([source, block.hash]);
            if previous_is_fresh {
                fresh.insert(block.previous);
            }
            let mut edges = BTreeSet::new();
            binding_edges(&mut edges, &fresh, &block, source.as_bytes(), None).unwrap();
            assert!(edges.contains(&(block.hash, source)));
            assert_eq!(edges.contains(&(source, block.previous)), previous_is_fresh);
            assert_eq!(edges.len(), if previous_is_fresh { 2 } else { 1 });
        }
    }
}
