use super::*;
use burst_governance::GovernableParam;
use burst_store::meta::{decode_payment_vote_limit, MetaStore, GOVERNANCE_COUNTING_LIMIT_KEY};
use burst_types::TxHash;

fn setup() -> (tempfile::TempDir, LmdbStore, ProtocolParams, BlockHash) {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let params = ProtocolParams {
        governance_proposal_duration_secs: 10,
        governance_exploration_duration_secs: 20,
        governance_propagation_buffer_secs: 1,
        governance_proposal_endorsements: 1,
        ..ProtocolParams::burst_defaults()
    };
    let genesis = crate::genesis_state::block(NetworkId::Dev, &params);
    let mut batch = store.tx_begin_write().unwrap();
    crate::payment_epoch::initialize(&mut batch, genesis.hash).unwrap();
    batch.commit().unwrap();
    crate::payment_authority::initialize(&store, NetworkId::Dev, &params).unwrap();
    (dir, store, params, genesis.hash)
}

fn block(
    params: &ProtocolParams,
    epoch: BlockHash,
    kind: BlockType,
    at: u64,
    link: BlockHash,
) -> StateBlock {
    let mut block = crate::genesis_state::block(NetworkId::Dev, params);
    block.payment_epoch = epoch;
    block.block_type = kind;
    block.timestamp = Timestamp::new(at);
    block.link = link;
    block.origin = TxHash::ZERO;
    block.hash = block.compute_hash();
    block
}

fn proposal(params: &ProtocolParams, epoch: BlockHash, at: u64) -> StateBlock {
    block(
        params,
        epoch,
        BlockType::GovernanceProposal,
        at,
        BlockHash::new(
            ProposalContent::ParameterChange {
                param: GovernableParam::BrnRate,
                new_value: 2,
            }
            .encode_compact()
            .unwrap(),
        ),
    )
}

fn limit(store: &LmdbStore, epoch: BlockHash) -> Option<u64> {
    decode_payment_vote_limit(
        &store
            .meta_store()
            .get_meta(GOVERNANCE_COUNTING_LIMIT_KEY)
            .unwrap(),
        epoch,
    )
    .unwrap()
}

#[test]
fn emergency_submission_immediately_fences_its_counting_cut_without_endorsements() {
    let (_dir, store, mut params, epoch) = setup();
    params.emergency_voting_duration_secs = 17;
    let batch = store.tx_begin_write().unwrap();
    let mut schedule = ConfirmedGovernance::initial(epoch, params.clone());
    let content = ProposalContent::Emergency {
        description: String::new(),
        param: GovernableParam::BrnRate,
        new_value: 2,
    };
    let proposal = block(
        &params,
        epoch,
        BlockType::GovernanceProposal,
        100,
        BlockHash::new(content.encode_compact().unwrap()),
    );
    assert_eq!(
        schedule.promise_limit(&batch, &proposal).unwrap(),
        Some(118)
    );
    schedule.promote(&batch, &proposal).unwrap();
    assert_eq!(
        decode_payment_vote_limit(&schedule.limit_bytes().unwrap(), epoch).unwrap(),
        Some(118)
    );
    for kind in [BlockType::GovernanceEndorse, BlockType::GovernanceWithdraw] {
        assert!(schedule
            .promote(&batch, &block(&params, epoch, kind, 101, proposal.hash))
            .is_err());
    }
    schedule.params.emergency_voting_duration_secs = 1;
    assert_eq!(
        decode_payment_vote_limit(&schedule.limit_bytes().unwrap(), epoch).unwrap(),
        Some(118)
    );
}

#[test]
fn invalid_deadline_cannot_partially_write_a_schedule() {
    let (_dir, store, params, epoch) = setup();
    let before = store.meta_store().get_meta(KEY).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let mut schedule = ConfirmedGovernance::load(&batch).unwrap().unwrap();
    let proposal = proposal(&params, epoch, 100);
    schedule.promote(&batch, &proposal).unwrap();
    schedule
        .promote(
            &batch,
            &block(
                &params,
                epoch,
                BlockType::GovernanceEndorse,
                101,
                proposal.hash,
            ),
        )
        .unwrap();
    schedule
        .proposals
        .get_mut(proposal.hash.as_bytes())
        .unwrap()
        .timing
        .voting_secs = u64::MAX;
    assert!(schedule.persist(&mut batch).is_err());
    batch.commit().unwrap();
    assert_eq!(store.meta_store().get_meta(KEY).unwrap(), before);
    assert_eq!(limit(&store, epoch), None);
}

#[test]
fn promise_limits_cover_first_support_and_zero_threshold_without_publishing_state() {
    let (_dir, store, params, epoch) = setup();
    let batch = store.tx_begin_write().unwrap();
    let mut schedule = ConfirmedGovernance::load(&batch).unwrap().unwrap();
    let proposal = proposal(&params, epoch, 100);
    assert_eq!(schedule.promise_limit(&batch, &proposal).unwrap(), None);
    schedule.promote(&batch, &proposal).unwrap();
    let support = block(
        &params,
        epoch,
        BlockType::GovernanceEndorse,
        101,
        proposal.hash,
    );
    assert_eq!(schedule.promise_limit(&batch, &support).unwrap(), Some(131));
    schedule.params.governance_proposal_endorsements = 0;
    assert_eq!(
        schedule.promise_limit(&batch, &proposal).unwrap(),
        Some(131)
    );
    drop(batch);
    assert_eq!(limit(&store, epoch), None);
}

#[test]
fn schedule_updates_are_atomic_and_withdrawal_only_lifts_a_committed_limit() {
    let (_dir, store, params, epoch) = setup();
    let proposal = proposal(&params, epoch, 100);
    let support = block(
        &params,
        epoch,
        BlockType::GovernanceEndorse,
        102,
        proposal.hash,
    );
    let withdrawal = block(
        &params,
        epoch,
        BlockType::GovernanceWithdraw,
        103,
        proposal.hash,
    );
    for commit in [false, true] {
        let mut batch = store.tx_begin_write().unwrap();
        let mut schedule = ConfirmedGovernance::load(&batch).unwrap().unwrap();
        schedule.promote(&batch, &proposal).unwrap();
        assert_eq!(
            decode_payment_vote_limit(&schedule.limit_bytes().unwrap(), epoch).unwrap(),
            None
        );
        schedule.promote(&batch, &support).unwrap();
        schedule.persist(&mut batch).unwrap();
        if commit {
            batch.commit().unwrap();
        } else {
            drop(batch);
        }
        assert_eq!(limit(&store, epoch), commit.then_some(131));
    }
    for commit in [false, true] {
        let mut batch = store.tx_begin_write().unwrap();
        let mut schedule = ConfirmedGovernance::load(&batch).unwrap().unwrap();
        schedule.promote(&batch, &withdrawal).unwrap();
        schedule.persist(&mut batch).unwrap();
        if commit {
            batch.commit().unwrap();
        } else {
            drop(batch);
        }
        assert_eq!(limit(&store, epoch), if commit { None } else { Some(131) });
    }
}

#[test]
fn independent_support_and_withdrawal_order_has_one_projection() {
    let (_dir, store, params, epoch) = setup();
    let proposal = proposal(&params, epoch, 100);
    let support = block(
        &params,
        epoch,
        BlockType::GovernanceEndorse,
        102,
        proposal.hash,
    );
    let withdrawal = block(
        &params,
        epoch,
        BlockType::GovernanceWithdraw,
        103,
        proposal.hash,
    );
    let batch = store.tx_begin_write().unwrap();
    let mut initial = ConfirmedGovernance::load(&batch).unwrap().unwrap();
    initial.promote(&batch, &proposal).unwrap();
    let mut first = initial.clone();
    first.promote(&batch, &support).unwrap();
    first.promote(&batch, &withdrawal).unwrap();
    let mut second = initial.clone();
    second.promote(&batch, &withdrawal).unwrap();
    second.promote(&batch, &support).unwrap();
    assert_eq!(
        bincode::serialize(&first).unwrap(),
        bincode::serialize(&second).unwrap()
    );
    for at in [103, 104] {
        let invalid_support = block(
            &params,
            epoch,
            BlockType::GovernanceEndorse,
            at,
            proposal.hash,
        );
        assert!(second.promote(&batch, &invalid_support).is_err());
    }
}

#[test]
fn earliest_active_proposal_wins_and_voting_survives_threshold_increase() {
    let (_dir, store, params, epoch) = setup();
    let batch = store.tx_begin_write().unwrap();
    let mut schedule = ConfirmedGovernance::load(&batch).unwrap().unwrap();
    for at in [100, 95] {
        let proposal = proposal(&params, epoch, at);
        schedule.promote(&batch, &proposal).unwrap();
        schedule
            .promote(
                &batch,
                &block(
                    &params,
                    epoch,
                    BlockType::GovernanceEndorse,
                    at + 1,
                    proposal.hash,
                ),
            )
            .unwrap();
    }
    assert_eq!(
        decode_payment_vote_limit(&schedule.limit_bytes().unwrap(), epoch).unwrap(),
        Some(126)
    );
    for proposal in schedule.proposals.values_mut() {
        proposal.exploration_started_at = Some(proposal.created_at + 10);
    }
    schedule.params.governance_proposal_endorsements = 5;
    assert_eq!(
        decode_payment_vote_limit(&schedule.limit_bytes().unwrap(), epoch).unwrap(),
        Some(126)
    );
    schedule.proposals.values_mut().for_each(|proposal| {
        proposal.timing = burst_governance::GovernanceTiming::new(
            &params,
            &ProposalContent::Emergency {
                description: "Timing".into(),
                param: GovernableParam::BrnRate,
                new_value: 2,
            },
        )
    });
    let expected = burst_governance::ballot::counting_time_from(Timestamp::new(105), true, &params)
        .unwrap()
        .as_secs();
    assert_eq!(
        decode_payment_vote_limit(&schedule.limit_bytes().unwrap(), epoch).unwrap(),
        Some(expected)
    );
    schedule
        .proposals
        .values_mut()
        .for_each(|proposal| proposal.exploration_started_at = Some(u64::MAX));
    assert!(schedule.limit_bytes().is_err());
}

#[test]
fn startup_rejects_consistent_but_fabricated_schedule_without_repair() {
    let (_dir, store, params, epoch) = setup();
    let mut batch = store.tx_begin_write().unwrap();
    let mut schedule = ConfirmedGovernance::load(&batch).unwrap().unwrap();
    let proposal = proposal(&params, epoch, 100);
    schedule.promote(&batch, &proposal).unwrap();
    schedule
        .promote(
            &batch,
            &block(
                &params,
                epoch,
                BlockType::GovernanceEndorse,
                101,
                proposal.hash,
            ),
        )
        .unwrap();
    schedule.persist(&mut batch).unwrap();
    batch.commit().unwrap();
    let before = store.meta_store().get_meta(KEY).unwrap();
    assert!(crate::payment_authority::initialize(&store, NetworkId::Dev, &params).is_err());
    assert_eq!(store.meta_store().get_meta(KEY).unwrap(), before);
    assert_eq!(limit(&store, epoch), Some(131));
}
