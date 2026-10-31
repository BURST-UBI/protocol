use super::*;
use crate::confirmed_history::{proposal_archive_key, ProposalWitness};
use burst_governance::{proposal::AmendmentOp, ProposalContent};
use burst_store::{account::AccountStore, block::BlockStore, meta::MetaStore};
use burst_types::Timestamp;
use burst_wallet_core::{
    governance_content::build_signed_proposal, transaction_builder::AccountState,
};

fn fixture() -> (
    ProtocolParams,
    Vec<StateBlock>,
    Vec<HistoryHead>,
    ProposalWitness,
) {
    let params = ProtocolParams {
        min_work_difficulty: 0,
        ..ProtocolParams::default()
    };
    let genesis = crate::genesis_state::block(NetworkId::Dev, &params);
    let account = AccountState {
        signing_grant: burst_types::BlockHash::ZERO,
        payment_epoch: genesis.hash,
        head: genesis.hash,
        block_count: 1,
        representative: genesis.account.clone(),
        brn_spent: 0,
        trst_balance: 0,
        release_sources: vec![],
    };
    let content = ProposalContent::ConstitutionalDiff {
        title: "History retains the signed edit".into(),
        operations: vec![AmendmentOp::ModifyArticle {
            article_number: 1,
            new_text: "🌍".repeat(50_000),
        }],
    };
    let (block, encoded) = build_signed_proposal(
        &account,
        &burst_crypto::keypair_from_seed(&[0; 32]),
        &content,
        params.params_hash(),
        Timestamp::new(10),
    )
    .unwrap();
    let heads = vec![HistoryHead {
        account: block.account.clone(),
        height: 2,
        hash: block.hash,
    }];
    let witness = ProposalWitness {
        block: block.hash,
        bytes: encoded.bytes().to_vec(),
    };
    (params, vec![genesis, block], heads, witness)
}

fn history(
    blocks: Vec<StateBlock>,
    heads: &[HistoryHead],
    contents: Vec<ProposalWitness>,
) -> Result<ConfirmedHistory, StoreError> {
    ConfirmedHistory::from_parts(
        NetworkId::Dev,
        blocks[0].hash,
        20,
        heads,
        &[],
        blocks,
        contents,
    )
}

#[test]
fn proposal_witnesses_are_exact_signed_dependencies_and_prefixes_drop_unneeded_bodies() {
    let (_, blocks, heads, witness) = fixture();
    let complete = history(blocks.clone(), &heads, vec![witness.clone()]).unwrap();
    assert_eq!(
        complete.proposal_content(&witness.block),
        Some(witness.bytes.as_slice())
    );
    assert!(history(blocks.clone(), &heads, vec![]).is_err());
    assert!(history(
        blocks.clone(),
        &heads,
        vec![witness.clone(), witness.clone()]
    )
    .is_err());
    for mutation in 0..4 {
        let mut wrong = witness.clone();
        match mutation {
            0 => wrong.block = blocks[0].hash,
            1 => wrong.bytes[0] ^= 1,
            2 => {
                wrong.bytes.pop();
            }
            _ => wrong.bytes.push(0),
        }
        assert!(history(blocks.clone(), &heads, vec![wrong]).is_err());
    }
    let mut unsigned = blocks.clone();
    unsigned[1].signature = burst_types::Signature([0; 64]);
    assert!(history(unsigned, &heads, vec![witness]).is_err());
    let genesis_heads = vec![HistoryHead {
        account: blocks[0].account.clone(),
        height: 1,
        hash: blocks[0].hash,
    }];
    let prefix = complete.prefix(10, &genesis_heads, &[]).unwrap();
    assert!(prefix.proposal_witnesses().is_empty());
    assert!(prefix.proposal_content(&blocks[1].hash).is_none());
    assert_eq!(
        complete
            .prefix(20, &heads, &genesis_heads)
            .unwrap()
            .commitment(),
        complete.commitment()
    );
}

#[test]
fn witness_order_is_canonical_and_one_body_cannot_stand_in_for_two_signed_proposals() {
    let (_, mut blocks, mut heads, first) = fixture();
    let mut second = blocks[1].clone();
    second.previous = blocks[1].hash;
    second.timestamp = Timestamp::new(11);
    second.hash = second.compute_hash();
    second.signature = burst_crypto::sign_message(
        second.hash.as_bytes(),
        &burst_crypto::keypair_from_seed(&[0; 32]).private,
    );
    heads[0].height = 3;
    heads[0].hash = second.hash;
    let second_witness = ProposalWitness {
        block: second.hash,
        bytes: first.bytes.clone(),
    };
    blocks.push(second);
    assert!(history(blocks.clone(), &heads, vec![first.clone(), first.clone()]).is_err());
    let complete = history(
        blocks.clone(),
        &heads,
        vec![first.clone(), second_witness.clone()],
    )
    .unwrap();
    let genesis = blocks[0].hash;
    blocks.reverse();
    let reordered = ConfirmedHistory::from_parts(
        NetworkId::Dev,
        genesis,
        20,
        &heads,
        &[],
        blocks,
        vec![second_witness, first.clone()],
    )
    .unwrap();
    assert_eq!(complete.commitment(), reordered.commitment());
    assert_eq!(
        bincode::serialize(complete.proposal_witnesses()).unwrap(),
        bincode::serialize(reordered.proposal_witnesses()).unwrap()
    );
    let parent = vec![HistoryHead {
        account: heads[0].account.clone(),
        height: 2,
        hash: first.block,
    }];
    let prefix = reordered.prefix(10, &parent, &[]).unwrap();
    assert_eq!(prefix.proposal_witnesses().len(), 1);
    assert_eq!(
        prefix.proposal_content(&first.block),
        Some(first.bytes.as_slice())
    );
}

#[test]
fn proposal_history_archives_atomically_and_recovers_without_live_sidecars() {
    let (params, blocks, heads, witness) = fixture();
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 32 << 20).unwrap();
    let complete = history(blocks.clone(), &heads, vec![witness.clone()]).unwrap();
    let archive = proposal_archive_key(witness.block);
    let mut batch = store.tx_begin_write().unwrap();
    persist_history(&mut batch, &complete).unwrap();
    drop(batch);
    assert!(store.meta_store().get_meta(&archive).is_err());
    assert!(store
        .meta_store()
        .get_meta(&format!("checkpoint_history/v1/{}", witness.block))
        .is_err());
    let mut batch = store.tx_begin_write().unwrap();
    persist_history(&mut batch, &complete).unwrap();
    batch.commit().unwrap();
    drop(store);
    let store = LmdbStore::open(directory.path(), 32, 32 << 20).unwrap();
    assert!(!store.block_store().exists(&witness.block).unwrap());
    assert!(store
        .meta_store()
        .get_meta(&crate::governance_content::key(witness.block))
        .is_err());
    let recovered =
        ConfirmedHistory::read_certified(&store, NetworkId::Dev, blocks[0].hash, 20, &heads, &[])
            .unwrap();
    assert_eq!(recovered.commitment(), complete.commitment());
    assert_eq!(
        recovered.proposal_content(&witness.block),
        Some(witness.bytes.as_slice())
    );
    // The archived body can now reconstruct an open proposal, but it does not
    // supply an approved ballot or authorize a governance boundary.
    let replay =
        crate::confirmed_replay::ConfirmedReplay::replay(&recovered, NetworkId::Dev, &params)
            .unwrap();
    assert_eq!(replay.params().params_hash(), params.params_hash());
    let anchor =
        crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(NetworkId::Dev, &params)
            .unwrap();
    assert!(
        crate::checkpoint_governance::PreparedGovernance::from_replay(
            &recovered, &anchor, &params, replay
        )
        .is_err()
    );

    // Corrupt live data must not be hidden by a valid historical copy.
    store
        .meta_store()
        .put_meta(&crate::governance_content::key(witness.block), b"corrupt")
        .unwrap();
    assert!(ConfirmedHistory::read_certified(
        &store,
        NetworkId::Dev,
        blocks[0].hash,
        20,
        &heads,
        &[]
    )
    .is_err());
    store
        .meta_store()
        .delete_meta(&crate::governance_content::key(witness.block))
        .unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta(&archive, b"corrupt archive").unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(persist_history(&mut batch, &complete).is_err());
    drop(batch);
    assert!(ConfirmedHistory::read_certified(
        &store,
        NetworkId::Dev,
        blocks[0].hash,
        20,
        &heads,
        &[]
    )
    .is_err());
}

#[test]
fn locally_cemented_history_requires_the_body_and_uses_the_same_commitment_as_detached_history() {
    let (_, blocks, heads, witness) = fixture();
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 32 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    for (index, block) in blocks.iter().enumerate() {
        batch
            .put_block_with_account(
                &block.hash,
                &bincode::serialize(block).unwrap(),
                &block.account,
                index as u64 + 1,
            )
            .unwrap();
    }
    let mut account =
        crate::genesis_state::account_to_write(None, blocks[0].account.clone(), blocks[1].hash)
            .unwrap();
    account.block_count = 2;
    account.confirmation_height = 2;
    batch.put_account_info(&account, false).unwrap();
    batch.commit().unwrap();
    assert_eq!(
        store
            .account_store()
            .get_account(&account.address)
            .unwrap()
            .head,
        blocks[1].hash
    );
    assert!(
        ConfirmedHistory::read(&store, NetworkId::Dev, blocks[0].hash, 20, &heads, &[]).is_err()
    );
    store
        .meta_store()
        .put_meta(
            &crate::governance_content::key(witness.block),
            &witness.bytes,
        )
        .unwrap();
    let local =
        ConfirmedHistory::read(&store, NetworkId::Dev, blocks[0].hash, 20, &heads, &[]).unwrap();
    let detached = history(blocks, &heads, vec![witness.clone()]).unwrap();
    assert_eq!(local.commitment(), detached.commitment());
    assert_eq!(
        local.proposal_content(&witness.block),
        detached.proposal_content(&witness.block)
    );
}
