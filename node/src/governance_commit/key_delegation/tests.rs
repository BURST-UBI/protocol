use super::*;
use burst_governance::{key_delegation::Revocation, Proposal};
use burst_transactions::governance::GovernanceVote;
use burst_types::{KeyPair, NetworkId};

struct Fixture {
    state: CandidateGovernance,
    keys: Vec<KeyPair>,
    people: Vec<WalletAddress>,
    metadata: HashMap<String, Vec<u8>>,
    proposal: TxHash,
}

impl Fixture {
    fn new() -> Self {
        let keys: Vec<_> = (1..=3)
            .map(|n| burst_crypto::keypair_from_seed(&[n; 32]))
            .collect();
        let people: Vec<_> = keys
            .iter()
            .map(|key| burst_crypto::derive_address(&key.public))
            .collect();
        let mut state = CandidateGovernance::initial(
            ProtocolParams::burst_defaults(),
            NetworkId::Dev,
            BlockHash::new([4; 32]),
        );
        let proposal = TxHash::new([5; 32]);
        state
            .governance
            .submit_proposal(
                Proposal {
                    hash: proposal,
                    proposer: people[0].clone(),
                    phase: GovernancePhase::Proposal,
                    content: ProposalContent::ParameterChange {
                        param: GovernableParam::BrnRate,
                        new_value: 1000,
                    },
                    endorsement_count: 1,
                    total_eligible_voters: 3,
                    exploration_started_at: Some(Timestamp::new(100)),
                    exploration_votes_yea: 0,
                    exploration_votes_nay: 0,
                    exploration_votes_abstain: 0,
                    cooldown_started_at: None,
                    promotion_started_at: None,
                    promotion_votes_yea: 0,
                    promotion_votes_nay: 0,
                    promotion_votes_abstain: 0,
                    round: 0,
                    created_at: Timestamp::new(1),
                    activation_at: None,
                },
                0,
                true,
                &state.params,
            )
            .unwrap();
        state.governance.get_proposal_mut(&proposal).unwrap().phase = GovernancePhase::Exploration;
        Self {
            state,
            keys,
            people,
            metadata: HashMap::new(),
            proposal,
        }
    }

    fn authority(&self, owner: usize, scope: &KeyScope) -> (String, DelegationAuthority) {
        let key = scoped::key(
            self.state.network,
            self.state.genesis,
            &self.people[owner],
            scope,
        )
        .unwrap();
        let authority = self
            .metadata
            .get(&key)
            .map(|bytes| {
                scoped::restore(
                    self.state.network,
                    self.state.genesis,
                    &self.people[owner],
                    scope,
                    bytes,
                )
                .unwrap()
            })
            .unwrap_or_else(|| {
                DelegationAuthority::new(self.state.network, self.state.genesis).unwrap()
            });
        (key, authority)
    }

    fn grant(&mut self, owner: usize, recipient: usize, scope: KeyScope) -> Grant {
        let (key, mut authority) = self.authority(owner, &scope);
        let grant = Grant::create(
            &self.keys[owner],
            self.keys[recipient].public.clone(),
            scope.clone(),
            authority.generation(&self.people[owner], &scope) + 1,
            self.state.network,
            self.state.genesis,
        )
        .unwrap();
        authority.apply_grant(grant.clone()).unwrap();
        self.metadata.insert(key, authority.save().unwrap());
        grant
    }

    fn revoke(&mut self, owner: usize, scope: KeyScope) {
        let (key, mut authority) = self.authority(owner, &scope);
        authority
            .apply_revocation(
                &Revocation::create(
                    &self.keys[owner],
                    scope.clone(),
                    authority.generation(&self.people[owner], &scope) + 1,
                    self.state.network,
                    self.state.genesis,
                )
                .unwrap(),
            )
            .unwrap();
        self.metadata.insert(key, authority.save().unwrap());
    }

    fn ballot(&mut self, grant: &Grant, recipient: usize) {
        let secondary = grant.decrypt(&self.keys[recipient].private).unwrap();
        let ballot =
            DelegatedBallot::sign(grant, &secondary, self.proposal, GovernanceVote::Yea).unwrap();
        self.state
            .governance
            .record_key_ballot(&ballot, Timestamp::new(101), &self.state.params)
            .unwrap();
    }

    fn projected(&self, eligible: &[WalletAddress]) -> DelegationEngine {
        self.state
            .delegation_at_cut(self.proposal, eligible, &self.metadata)
            .unwrap()
    }
}

#[test]
fn key_projection_requires_verified_chain_and_live_grant_generation() {
    let mut f = Fixture::new();
    let parameter = KeyScope::Parameter(GovernableParam::BrnRate);
    f.grant(0, 1, parameter.clone());
    let downstream = f.grant(1, 2, parameter.clone());
    f.ballot(&downstream, 2);
    assert_eq!(
        f.projected(&f.people).key_ballot(&f.people[0]),
        Some(Some(GovernanceVote::Yea))
    );
    for missing in [1, 2] {
        let eligible: Vec<_> = f
            .people
            .iter()
            .enumerate()
            .filter(|(n, _)| *n != missing)
            .map(|(_, p)| p.clone())
            .collect();
        assert_eq!(f.projected(&eligible).key_ballot(&f.people[0]), Some(None));
    }
    f.grant(1, 2, parameter.clone());
    assert_eq!(f.projected(&f.people).key_ballot(&f.people[0]), Some(None));
    f.revoke(1, KeyScope::Proposal(f.proposal));
    assert_eq!(f.projected(&f.people).key_ballot(&f.people[0]), Some(None));
}

#[test]
fn key_projection_bounds_cycles_depth_and_honors_proposal_precedence() {
    let mut f = Fixture::new();
    let parameter = KeyScope::Parameter(GovernableParam::BrnRate);
    f.grant(0, 1, parameter.clone());
    f.grant(1, 0, parameter.clone());
    assert_eq!(f.projected(&f.people).key_ballot(&f.people[0]), Some(None));
    let downstream = f.grant(1, 2, parameter.clone());
    f.ballot(&downstream, 2);
    f.state.delegation = DelegationEngine::new(1);
    assert_eq!(f.projected(&f.people).key_ballot(&f.people[0]), Some(None));
    f.state.delegation = DelegationEngine::new(2);
    assert_eq!(
        f.projected(&f.people).key_ballot(&f.people[0]),
        Some(Some(GovernanceVote::Yea))
    );
    f.revoke(0, KeyScope::Proposal(f.proposal));
    assert_eq!(f.projected(&f.people).key_ballot(&f.people[0]), Some(None));
    let specific = f.grant(0, 2, KeyScope::Proposal(f.proposal));
    f.ballot(&specific, 2);
    assert_eq!(
        f.projected(&f.people).key_ballot(&f.people[0]),
        Some(Some(GovernanceVote::Yea))
    );
}
