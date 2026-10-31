use super::*;
use burst_governance::{signing_delegation::Revocation, Proposal};
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
        Self::with_population(3)
    }

    fn with_population(population: usize) -> Self {
        let keys: Vec<_> = (1..=population)
            .map(|n| {
                let mut seed = [n as u8; 32];
                if n > 255 {
                    seed[..8].copy_from_slice(&(n as u64).to_le_bytes());
                }
                burst_crypto::keypair_from_seed(&seed)
            })
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
                    total_eligible_voters: u32::try_from(population).unwrap(),
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
    let mut fixture = Fixture::new();
    let parameter = KeyScope::Parameter(GovernableParam::BrnRate);
    fixture.grant(0, 1, parameter.clone());
    let downstream = fixture.grant(1, 2, parameter.clone());
    fixture.ballot(&downstream, 2);
    assert_eq!(
        fixture
            .projected(&fixture.people)
            .key_ballot(&fixture.people[0]),
        Some(Some(GovernanceVote::Yea))
    );
    for missing in [1, 2] {
        let eligible: Vec<_> = fixture
            .people
            .iter()
            .enumerate()
            .filter(|(n, _)| *n != missing)
            .map(|(_, p)| p.clone())
            .collect();
        assert_eq!(
            fixture.projected(&eligible).key_ballot(&fixture.people[0]),
            Some(None)
        );
    }
    fixture.grant(1, 2, parameter.clone());
    assert_eq!(
        fixture
            .projected(&fixture.people)
            .key_ballot(&fixture.people[0]),
        Some(None)
    );
    fixture.revoke(1, KeyScope::Proposal(fixture.proposal));
    assert_eq!(
        fixture
            .projected(&fixture.people)
            .key_ballot(&fixture.people[0]),
        Some(None)
    );
}

#[test]
fn key_projection_bounds_cycles_depth_and_honors_proposal_precedence() {
    let mut fixture = Fixture::new();
    let parameter = KeyScope::Parameter(GovernableParam::BrnRate);
    fixture.grant(0, 1, parameter.clone());
    fixture.grant(1, 0, parameter.clone());
    assert_eq!(
        fixture
            .projected(&fixture.people)
            .key_ballot(&fixture.people[0]),
        Some(None)
    );
    let downstream = fixture.grant(1, 2, parameter.clone());
    fixture.ballot(&downstream, 2);
    fixture.state.delegation = DelegationEngine::new(1);
    assert_eq!(
        fixture
            .projected(&fixture.people)
            .key_ballot(&fixture.people[0]),
        Some(None)
    );
    fixture.state.delegation = DelegationEngine::new(2);
    assert_eq!(
        fixture
            .projected(&fixture.people)
            .key_ballot(&fixture.people[0]),
        Some(Some(GovernanceVote::Yea))
    );
    fixture.revoke(0, KeyScope::Proposal(fixture.proposal));
    assert_eq!(
        fixture
            .projected(&fixture.people)
            .key_ballot(&fixture.people[0]),
        Some(None)
    );
    let specific = fixture.grant(0, 2, KeyScope::Proposal(fixture.proposal));
    fixture.ballot(&specific, 2);
    assert_eq!(
        fixture
            .projected(&fixture.people)
            .key_ballot(&fixture.people[0]),
        Some(Some(GovernanceVote::Yea))
    );
}

#[test]
fn cached_prefix_projection_matches_legacy_walk_for_depth_order_and_revocation() {
    fn compare(fixture: &Fixture) {
        for mask in 0..8 {
            for reverse in [false, true] {
                let mut people: Vec<_> = fixture
                    .people
                    .iter()
                    .enumerate()
                    .filter(|(n, _)| mask & (1 << n) != 0)
                    .map(|(_, p)| p.clone())
                    .collect();
                if reverse {
                    people.reverse();
                }
                let actual = fixture.projected(&people);
                let eligible: HashSet<_> = people.iter().collect();
                let param = Some(&GovernableParam::BrnRate);
                for owner in &people {
                    if fixture
                        .state
                        .scoped_grant(owner, fixture.proposal, param, &fixture.metadata)
                        .unwrap()
                        .is_none()
                    {
                        assert_eq!(actual.key_ballot(owner), None);
                        continue;
                    }
                    let mut current = owner.clone();
                    let mut seen = HashSet::new();
                    let mut expected = None;
                    for _ in 0..fixture.state.delegation.max_depth() {
                        if !eligible.contains(&current) || !seen.insert(current.clone()) {
                            break;
                        }
                        let Some(Some(grant)) = fixture
                            .state
                            .scoped_grant(&current, fixture.proposal, param, &fixture.metadata)
                            .unwrap()
                        else {
                            break;
                        };
                        let recipient = burst_crypto::derive_address(&grant.delegate);
                        if !eligible.contains(&recipient) {
                            break;
                        }
                        if let Some(vote) = fixture.state.governance.key_ballot(
                            fixture.proposal,
                            &current,
                            grant.id().unwrap(),
                        ) {
                            expected = Some(vote);
                            break;
                        }
                        current = recipient;
                    }
                    assert_eq!(
                        actual.key_ballot(owner),
                        Some(expected),
                        "mask {mask}, reverse {reverse}, depth {}",
                        fixture.state.delegation.max_depth()
                    );
                }
            }
        }
    }
    let mut fixture = Fixture::new();
    let scope = KeyScope::Parameter(GovernableParam::BrnRate);
    fixture.grant(0, 1, scope.clone());
    let downstream = fixture.grant(1, 2, scope.clone());
    fixture.grant(2, 0, scope);
    for stage in 0..3 {
        if stage == 1 {
            fixture.ballot(&downstream, 2);
        }
        if stage == 2 {
            fixture.revoke(1, KeyScope::Proposal(fixture.proposal));
        }
        for depth in [0, 1, 2, 3, 10] {
            fixture.state.delegation = DelegationEngine::new(depth);
            compare(&fixture);
        }
    }
}

#[test]
fn cut_grants_authenticate_once_and_keep_negative_results_distinct() {
    let mut fixture = Fixture::new();
    let scope = KeyScope::Parameter(GovernableParam::BrnRate);
    let grant = fixture.grant(0, 1, scope);
    fixture.revoke(1, KeyScope::Proposal(fixture.proposal));
    let mut reads = HashMap::<WalletAddress, usize>::new();
    let mut cache = grants::CutGrants::new(|owner: &WalletAddress| {
        *reads.entry(owner.clone()).or_default() += 1;
        fixture.state.scoped_grant(
            owner,
            fixture.proposal,
            Some(&GovernableParam::BrnRate),
            &fixture.metadata,
        )
    });
    for _ in 0..128 {
        assert_eq!(
            cache
                .get(&fixture.people[0])
                .unwrap()
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .id()
                .unwrap(),
            grant.id().unwrap()
        );
        assert_eq!(
            cache.hop(&fixture.people[0], |_| true).unwrap(),
            Some((&fixture.people[1], grant.id().unwrap()))
        );
        assert!(cache.hop(&fixture.people[1], |_| true).unwrap().is_none());
        assert!(cache.hop(&fixture.people[2], |_| true).unwrap().is_none());
        assert_eq!(cache.derived_counts(), (1, 1));
        assert!(matches!(cache.get(&fixture.people[1]).unwrap(), Some(None)));
        assert!(cache.get(&fixture.people[2]).unwrap().is_none());
    }
    drop(cache);
    assert_eq!(reads.len(), 3);
    assert!(reads.values().all(|count| *count == 1));

    // A fresh projection must see the revocation added to the fixture metadata.
    fixture.revoke(0, KeyScope::Proposal(fixture.proposal));
    let mut next_cut = grants::CutGrants::new(|owner: &WalletAddress| {
        fixture.state.scoped_grant(
            owner,
            fixture.proposal,
            Some(&GovernableParam::BrnRate),
            &fixture.metadata,
        )
    });
    assert!(matches!(
        next_cut.get(&fixture.people[0]).unwrap(),
        Some(None)
    ));
}

#[test]
fn cut_grants_load_lazily_and_propagate_malformed_scope_errors() {
    let mut fixture = Fixture::new();
    let key = scoped::key(
        fixture.state.network,
        fixture.state.genesis,
        &fixture.people[1],
        &KeyScope::Proposal(fixture.proposal),
    )
    .unwrap();
    fixture.metadata.insert(key, vec![0xff]);
    let mut cache = grants::CutGrants::new(|owner: &WalletAddress| {
        fixture.state.scoped_grant(
            owner,
            fixture.proposal,
            Some(&GovernableParam::BrnRate),
            &fixture.metadata,
        )
    });
    assert!(cache.get(&fixture.people[0]).unwrap().is_none());
    assert!(cache.get(&fixture.people[1]).is_err());
    assert!(fixture
        .state
        .delegation_at_cut(fixture.proposal, &fixture.people, &fixture.metadata)
        .is_err());
}

#[test]
fn cut_grant_derivation_preserves_lazy_id_errors_and_recipient_checks() {
    let mut fixture = Fixture::new();
    let mut grant = fixture.grant(0, 1, KeyScope::Parameter(GovernableParam::BrnRate));
    grant.genesis = BlockHash::ZERO;
    assert!(grant.id().is_err());
    let mut cache = grants::CutGrants::new(|_: &WalletAddress| {
        Ok(Some(Some(grants::CutGrant::Raw(grant.clone()))))
    });
    for _ in 0..3 {
        assert!(cache.hop(&fixture.people[0], |_| false).unwrap().is_none());
        assert_eq!(cache.derived_counts(), (1, 0));
    }
    assert!(cache.hop(&fixture.people[0], |_| true).is_err());
    assert_eq!(cache.derived_counts(), (1, 1));
    assert!(cache.hop(&fixture.people[0], |_| true).is_err());
    assert!(cache.hop(&fixture.people[0], |_| false).unwrap().is_none());
}

#[test]
fn parameter_cache_keeps_proposal_overrides_ballots_and_cut_changes_separate() {
    let mut fixture = Fixture::new();
    let first = fixture.proposal;
    let others: Vec<_> = (41..=44).map(|n| TxHash::new([n; 32])).collect();
    for (index, hash) in others.iter().enumerate() {
        let mut proposal = fixture
            .state
            .governance
            .get_proposal(&first)
            .unwrap()
            .clone();
        proposal.hash = *hash;
        proposal.phase = GovernancePhase::Proposal;
        if index == 1 {
            proposal.content = ProposalContent::ParameterChange {
                param: GovernableParam::TrstExpirySecs,
                new_value: 1000,
            };
        }
        fixture
            .state
            .governance
            .submit_proposal(proposal, 0, true, &fixture.state.params)
            .unwrap();
        fixture
            .state
            .governance
            .get_proposal_mut(hash)
            .unwrap()
            .phase = GovernancePhase::Exploration;
    }
    let common = fixture.grant(0, 1, KeyScope::Parameter(GovernableParam::BrnRate));
    fixture.ballot(&common, 1);
    let different = fixture.grant(0, 2, KeyScope::Parameter(GovernableParam::TrstExpirySecs));
    let specific = fixture.grant(0, 2, KeyScope::Proposal(others[2]));
    for (proposal, grant, recipient, vote) in [
        (others[0], &common, 1, GovernanceVote::Nay),
        (others[1], &different, 2, GovernanceVote::Abstain),
        (others[2], &specific, 2, GovernanceVote::Yea),
    ] {
        let secondary = grant.decrypt(&fixture.keys[recipient].private).unwrap();
        let ballot = DelegatedBallot::sign(grant, &secondary, proposal, vote).unwrap();
        fixture
            .state
            .governance
            .record_key_ballot(&ballot, Timestamp::new(101), &fixture.state.params)
            .unwrap();
    }
    fixture.revoke(0, KeyScope::Proposal(others[3]));
    let mut single = fixture.state.single_projection(&fixture.metadata);
    assert!(single.delegation(first, &fixture.people).is_ok());
    assert!(single.parameters.is_none());
    assert!(single.owners.is_none());
    drop(single);
    let mut cut = fixture.state.projection_cut(&fixture.metadata);
    let mut retained = None;
    for (proposal, vote) in [
        (first, Some(GovernanceVote::Yea)),
        (others[0], Some(GovernanceVote::Nay)),
        (others[1], Some(GovernanceVote::Abstain)),
        (others[2], Some(GovernanceVote::Yea)),
        (others[3], None),
    ] {
        let projected = cut.delegation(proposal, &fixture.people).unwrap();
        let independent = fixture
            .state
            .delegation_at_cut(proposal, &fixture.people, &fixture.metadata)
            .unwrap();
        for person in &fixture.people {
            assert_eq!(projected.key_ballot(person), independent.key_ballot(person));
        }
        assert_eq!(projected.key_ballot(&fixture.people[0]), Some(vote));
        if proposal == first {
            retained = Some(projected);
        }
    }
    assert_eq!(cut.parameters.as_ref().unwrap().len(), 6); // Two parameters, not five proposal scopes.
    drop(cut);
    fixture.revoke(0, KeyScope::Parameter(GovernableParam::BrnRate));
    assert_eq!(
        fixture
            .projected(&fixture.people)
            .key_ballot(&fixture.people[0]),
        Some(None)
    );
    assert_eq!(
        retained.unwrap().key_ballot(&fixture.people[0]),
        Some(Some(GovernanceVote::Yea))
    );
    let malformed = scoped::key(
        fixture.state.network,
        fixture.state.genesis,
        &fixture.people[0],
        &KeyScope::Proposal(others[0]),
    )
    .unwrap();
    fixture.metadata.insert(malformed, vec![0xff]);
    let mut cut = fixture.state.projection_cut(&fixture.metadata);
    assert!(cut.delegation(first, &fixture.people).is_ok());
    assert!(cut.delegation(others[0], &fixture.people).is_err());
}

// Diagnostic-only spans are thread-local so parallel tests cannot mix cuts.
#[derive(Default, serde::Serialize)]
struct ProjectionStages {
    // First five: scope-key total, restore, recipient, ID, map write.
    // Stages 5–6 are children of scope-key total; stage 7 is electorate membership construction.
    nanos: [u128; 8],
    calls: [usize; 8],
}
thread_local! {
    static PROJECTION_STAGES: std::cell::RefCell<Option<ProjectionStages>> = const {
        std::cell::RefCell::new(None)
    };
}
pub(super) struct ProjectionSpan {
    stage: usize,
    started_at: Option<std::time::Instant>,
}
pub(super) fn projection_span(stage: usize) -> ProjectionSpan {
    ProjectionSpan {
        stage,
        started_at: PROJECTION_STAGES
            .with(|slot| slot.borrow().as_ref().map(|_| std::time::Instant::now())),
    }
}
impl Drop for ProjectionSpan {
    fn drop(&mut self) {
        if let Some(start) = self.started_at {
            let elapsed = start.elapsed().as_nanos();
            PROJECTION_STAGES.with(|slot| {
                if let Some(stages) = slot.borrow_mut().as_mut() {
                    stages.nanos[self.stage] += elapsed;
                    stages.calls[self.stage] += 1;
                }
            });
        }
    }
}

pub(crate) fn measure_projection_work<T>(
    operation: impl FnOnce() -> T,
) -> (T, [usize; 8], [u128; 8]) {
    PROJECTION_STAGES.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(ProjectionStages::default());
    });
    let result = operation();
    let stages = PROJECTION_STAGES.with(|slot| slot.borrow_mut().take().unwrap());
    (result, stages.calls, stages.nanos)
}

fn authenticated_projection_case(population: usize, depth: usize, shape: &str, samples: usize) {
    use burst_governance::ballot::BallotElectorate;
    use std::time::Instant;
    let setup = Instant::now();
    let mut fixture = Fixture::with_population(population);
    fixture.state.delegation = DelegationEngine::new(depth);
    let mut edges = vec![None; population];
    let mut ballot_owner = None;
    for owner in 0..population {
        let recipient = match shape {
            "none" => None,
            "sparse" => (owner % 10 == 0).then_some((owner + 1) % population),
            "shared_vote" => Some(if owner == population - 2 {
                population - 1
            } else {
                population - 2
            }),
            "cycle" => Some((owner + 1) % population),
            _ => panic!("unknown authenticated projection shape"),
        };
        if let Some(recipient) = recipient {
            edges[owner] = Some(recipient);
            let grant = fixture.grant(
                owner,
                recipient,
                KeyScope::Parameter(GovernableParam::BrnRate),
            );
            if shape == "shared_vote" && owner == population - 2 {
                fixture.ballot(&grant, recipient);
                ballot_owner = Some(owner);
            }
        }
    }
    // Independent bounded graph expectation: every supplied grant is signed and
    // every recipient eligible. An absent grant leaves legacy fallback untouched.
    let expected: Vec<_> = (0..population)
        .map(|owner| {
            edges[owner]?;
            let mut current = owner;
            let mut seen = HashSet::new();
            for _ in 0..depth {
                if !seen.insert(current) {
                    break;
                }
                let Some(next) = edges[current] else { break };
                if Some(current) == ballot_owner {
                    return Some(Some(GovernanceVote::Yea));
                }
                current = next;
            }
            Some(None)
        })
        .collect();
    let expected_yea = expected
        .iter()
        .filter(|vote| **vote == Some(Some(GovernanceVote::Yea)))
        .count();
    let target = fixture
        .state
        .governance
        .get_proposal(&fixture.proposal)
        .unwrap();
    let cutoff = fixture
        .state
        .governance
        .counting_time(target, &fixture.state.params)
        .unwrap();
    let history = BlockHash::new([17; 32]);
    let setup_ms = setup.elapsed().as_secs_f64() * 1000.0;
    for sample in 0..samples {
        let start = Instant::now();
        let projected = fixture.projected(&fixture.people);
        let projection_ms = start.elapsed().as_secs_f64() * 1000.0;
        for (owner, expected) in fixture.people.iter().zip(&expected) {
            assert_eq!(projected.key_ballot(owner), *expected);
        }
        let electorate = BallotElectorate::new(fixture.people.as_slice());
        let prepare = || {
            fixture
                .state
                .governance
                .prepare_ballot_with_electorate(
                    &fixture.proposal,
                    history,
                    cutoff,
                    &fixture.state.params,
                    &projected,
                    &electorate,
                )
                .unwrap()
        };
        let start = Instant::now();
        let first = prepare();
        let fresh_ballot_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        let repeated = prepare();
        let cached_ballot_ms = start.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(first.next_proposal(), repeated.next_proposal());
        assert_eq!(first.commitment().unwrap(), repeated.commitment().unwrap());
        assert_eq!(
            first.next_proposal().exploration_votes_yea as usize,
            expected_yea
        );
        assert_eq!(first.next_proposal().exploration_votes_nay, 0);
        assert_eq!(first.next_proposal().exploration_votes_abstain, 0);
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let memory: Vec<_> = status
            .lines()
            .filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:"))
            .collect();
        println!(
            "authenticated_projection {}",
            serde_json::json!({
                "population":population, "depth":depth, "shape":shape, "sample":sample,
                "setup_ms":setup_ms, "projection_ms":projection_ms,
                "fresh_ballot_ms":fresh_ballot_ms, "cached_ballot_ms":cached_ballot_ms,
                "metadata_rows":fixture.metadata.len(), "expected_yea":expected_yea, "memory":memory,
            })
        );
        if population == 12 || std::env::var_os("BURST_GOV_PROBE_STAGES").is_some() {
            PROJECTION_STAGES.with(|slot| {
                assert!(slot.borrow().is_none());
                *slot.borrow_mut() = Some(ProjectionStages::default());
            });
            let start = Instant::now();
            let instrumented = fixture.projected(&fixture.people);
            let instrumented_ms = start.elapsed().as_secs_f64() * 1000.0;
            let stages = PROJECTION_STAGES.with(|slot| slot.borrow_mut().take().unwrap());
            let granted = edges.iter().filter(|edge| edge.is_some()).count();
            assert_eq!(
                stages.calls,
                [
                    population * 2,
                    granted,
                    granted,
                    granted,
                    granted,
                    population,
                    population * 2,
                    1
                ]
            );
            for (owner, expected) in fixture.people.iter().zip(&expected) {
                assert_eq!(instrumented.key_ballot(owner), *expected);
            }
            println!(
                "authenticated_stages {}",
                serde_json::json!({
                    "population":population, "depth":depth, "shape":shape, "sample":sample,
                    "instrumented_ms":instrumented_ms, "stages":stages,
                    "order":["scope_key", "restore_authentication", "recipient", "grant_id", "projection_write", "owner_identity", "scope_encode_hash", "electorate_membership"],
                })
            );
        }
    }
}

#[test]
fn authenticated_projection_stages_match_independent_votes() {
    for shape in ["none", "sparse", "shared_vote", "cycle"] {
        authenticated_projection_case(12, 2, shape, 1);
    }
}

#[test]
#[ignore = "explicit capped-host authenticated projection diagnostic"]
fn authenticated_projection_scaling() {
    let population = std::env::var("BURST_GOV_PROBE_POPULATION")
        .unwrap()
        .parse()
        .unwrap();
    let depth = std::env::var("BURST_GOV_PROBE_DEPTH")
        .unwrap()
        .parse()
        .unwrap();
    let shape = std::env::var("BURST_GOV_PROBE_SHAPE").unwrap();
    assert!([100, 1000].contains(&population));
    assert!([10, 128].contains(&depth));
    authenticated_projection_case(population, depth, &shape, 3);
}

fn parameter_cache_population_case(population: usize, parameter_count: usize, sparse: bool) {
    parameter_cache_grant_case(population, parameter_count, sparse.then_some(100));
}

fn parameter_cache_grant_case(population: usize, parameter_count: usize, stride: Option<usize>) {
    use std::time::Instant;
    fn memory() -> Vec<String> {
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:"))
            .map(str::to_owned)
            .collect()
    }
    let start = Instant::now();
    let mut fixture = Fixture::with_population(population);
    let parameters = &GovernableParam::ALL[..parameter_count];
    let mut proposals = Vec::new();
    for index in 0..parameter_count * 2 {
        let hash = TxHash::new([41 + index as u8; 32]);
        let mut proposal = fixture
            .state
            .governance
            .get_proposal(&fixture.proposal)
            .unwrap()
            .clone();
        proposal.hash = hash;
        proposal.phase = GovernancePhase::Proposal;
        proposal.content = ProposalContent::ParameterChange {
            param: parameters[index % parameter_count].clone(),
            new_value: 1000,
        };
        fixture
            .state
            .governance
            .submit_proposal(proposal, 0, true, &fixture.state.params)
            .unwrap();
        fixture
            .state
            .governance
            .get_proposal_mut(&hash)
            .unwrap()
            .phase = GovernancePhase::Exploration;
        proposals.push(hash);
    }
    let granted = if let Some(stride) = stride {
        for parameter in parameters {
            for owner in (0..population).step_by(stride) {
                fixture.grant(
                    owner,
                    (owner + 1) % population,
                    KeyScope::Parameter(parameter.clone()),
                );
            }
        }
        population.div_ceil(stride)
    } else {
        0
    };
    let setup_ms = start.elapsed().as_secs_f64() * 1000.0;
    let before = memory();
    let mut cut = fixture.state.projection_cut(&fixture.metadata);
    let mut projection_ms = 0.0;
    let mut checkpoints = Vec::new();
    let ((), calls, nanos) = measure_projection_work(|| {
        for (index, proposal) in proposals.iter().enumerate() {
            let started = Instant::now();
            let projection = cut.delegation(*proposal, &fixture.people).unwrap();
            projection_ms += started.elapsed().as_secs_f64() * 1000.0;
            for (owner, person) in fixture.people.iter().enumerate() {
                let expected = if stride.is_some_and(|stride| owner % stride == 0) {
                    Some(None)
                } else {
                    None
                };
                assert_eq!(projection.key_ballot(person), expected);
            }
            let cache = cut.parameters.as_ref().unwrap();
            assert_eq!(cache.len(), population * (index + 1).min(parameter_count));
            checkpoints.push(serde_json::json!({"proposal":index + 1,
                "entries":cache.len(), "storage_counts":cache.storage_counts(), "memory":memory()}));
        }
    });
    assert_eq!(calls[0], population * parameter_count * 3);
    assert_eq!(calls[1], granted * parameter_count);
    assert_eq!(&calls[2..5], &[granted * parameter_count * 2; 3]);
    assert_eq!(calls[5], population);
    let indexed = matches!(cut.records, Some(Some(_)));
    if stride == Some(1) && parameter_count > 1 {
        assert!(
            !indexed,
            "dense metadata exceeds the electorate-sized index budget"
        );
    }
    assert_eq!(calls[6], if indexed { 0 } else { calls[0] });
    assert_eq!(calls[7], 1);
    assert_eq!(cut.owners.as_ref().unwrap().len(), population);
    let cache = cut.parameters.as_ref().unwrap();
    assert_eq!(
        cache.negative_count(),
        (population - granted) * parameter_count
    );
    let (absent_owners, _, records, _) = cache.storage_counts();
    assert_eq!(absent_owners, population - granted);
    assert_eq!(records, granted * parameter_count);
    println!(
        "parameter_cache_population {}",
        serde_json::json!({
            "population":population, "parameters":parameter_count, "proposals":proposals.len(),
            "sparse":stride == Some(100), "grant_stride":stride, "indexed":indexed,
            "grants":granted * parameter_count, "setup_ms":setup_ms,
            "projection_ms":projection_ms, "calls":calls, "nanos":nanos,
            "stage_order":["scope_key_total", "restore", "recipient", "grant_id", "projection_write", "owner_identity", "scope_encode_hash", "electorate_membership"],
            "nested_stages":"owner_identity and scope_encode_hash are included in scope_key_total; do not add them again",
            "owner_identities":cut.owners.as_ref().unwrap().len(),
            "cache_entries":cache.len(), "storage_counts":cache.storage_counts(),
            "test_key_bytes":std::mem::size_of::<(WalletAddress, GovernableParam)>(),
            "test_value_bytes":std::mem::size_of::<grants::ScopedGrant>(),
            "absence_owner_key_bytes":std::mem::size_of::<WalletAddress>(),
            "absence_value_bytes":GovernableParam::ALL.len().div_ceil(64) * std::mem::size_of::<u64>(),
            "memory_before":before, "checkpoints":checkpoints,
        })
    );
}

#[test]
fn parameter_cache_population_counts_distinct_scopes_and_preserves_absence() {
    for parameters in [1, 8] {
        for sparse in [false, true] {
            parameter_cache_population_case(12, parameters, sparse);
        }
    }
}

#[test]
fn parameter_cache_dense_fallback_authenticates_each_scope_once() {
    parameter_cache_grant_case(12, 8, Some(1));
}

#[test]
#[ignore = "explicit capped-host dense parameter cache diagnostic"]
fn parameter_cache_dense_scaling() {
    let population = std::env::var("BURST_GOV_PROBE_POPULATION")
        .unwrap()
        .parse()
        .unwrap();
    assert!([1000, 10000].contains(&population));
    parameter_cache_grant_case(population, 8, Some(1));
}

#[test]
#[ignore = "explicit capped-host parameter cache population diagnostic"]
fn parameter_cache_population_scaling() {
    let population = std::env::var("BURST_GOV_PROBE_POPULATION")
        .unwrap()
        .parse()
        .unwrap();
    let parameters = std::env::var("BURST_GOV_PROBE_PARAMETERS")
        .unwrap()
        .parse()
        .unwrap();
    let sparse = std::env::var("BURST_GOV_PROBE_SPARSE")
        .unwrap()
        .parse()
        .unwrap();
    assert!([1000, 100000].contains(&population));
    assert!([1, 8].contains(&parameters));
    parameter_cache_population_case(population, parameters, sparse);
}

#[test]
fn parameter_cache_compacts_all_absences_without_hiding_revocations_or_errors() {
    let mut fixture = Fixture::new();
    // Every parameter discriminant must fit the absence bitset indexing.
    let indices: HashSet<_> = GovernableParam::ALL
        .iter()
        .map(|param| param.clone() as usize)
        .collect();
    assert_eq!(indices, (0..GovernableParam::ALL.len()).collect());
    let revoked = GovernableParam::ALL.last().unwrap().clone();
    fixture.revoke(1, KeyScope::Parameter(revoked.clone()));
    let corrupt_param = GovernableParam::TrstExpirySecs;
    let corrupt = scoped::key(
        fixture.state.network,
        fixture.state.genesis,
        &fixture.people[2],
        &KeyScope::Parameter(corrupt_param.clone()),
    )
    .unwrap();
    fixture.metadata.insert(corrupt, vec![0xff]);
    let mut cut = fixture.state.projection_cut(&fixture.metadata);
    for _ in 0..2 {
        for param in GovernableParam::ALL {
            assert!(cut
                .scoped_grant(&fixture.people[0], fixture.proposal, Some(param))
                .unwrap()
                .is_none());
        }
        assert!(matches!(
            cut.scoped_grant(&fixture.people[1], fixture.proposal, Some(&revoked))
                .unwrap(),
            Some(None)
        ));
        // Warming absence for a different owner must not mask corrupt metadata;
        // failures are not cached as absence on the next attempt either.
        assert!(cut
            .scoped_grant(&fixture.people[2], fixture.proposal, Some(&corrupt_param))
            .is_err());
    }
    let cache = cut.parameters.as_ref().unwrap();
    assert_eq!(cache.negative_count(), GovernableParam::ALL.len());
    assert_eq!(cache.len(), GovernableParam::ALL.len() + 1);
    let (owners, _, records, _) = cache.storage_counts();
    assert_eq!((owners, records), (1, 1));
}

#[test]
fn owner_identity_cache_is_lazy_cut_bound_and_preserves_errors() {
    let mut fixture = Fixture::new();
    let invalid = WalletAddress::new("brst_invalid");
    let mut cut = fixture.state.projection_cut(&fixture.metadata);
    assert!(cut.owners.as_ref().unwrap().is_empty());
    let ((), calls, _) = measure_projection_work(|| {
        for proposal in [fixture.proposal, TxHash::new([77; 32]), fixture.proposal] {
            assert!(cut
                .scoped_grant(&fixture.people[0], proposal, None)
                .unwrap()
                .is_none());
        }
    });
    assert_eq!((calls[0], calls[5], calls[6]), (3, 1, 3));
    assert_eq!(cut.owners.as_ref().unwrap().len(), 1);
    for _ in 0..2 {
        let shared = cut
            .scoped_grant(&invalid, TxHash::ZERO, None)
            .err()
            .unwrap();
        let independent = fixture
            .state
            .single_projection(&fixture.metadata)
            .scoped_grant(&invalid, TxHash::ZERO, None)
            .err()
            .unwrap();
        assert_eq!(shared, independent);
        assert_eq!(shared, "invalid delegation owner");
        assert!(!cut.owners.as_ref().unwrap().contains_key(&invalid));
    }
    let shared = cut
        .scoped_grant(&fixture.people[0], TxHash::ZERO, None)
        .err()
        .unwrap();
    let independent = fixture
        .state
        .single_projection(&fixture.metadata)
        .scoped_grant(&fixture.people[0], TxHash::ZERO, None)
        .err()
        .unwrap();
    assert_eq!(shared, independent); // Warm identity cannot bypass scope validation.
    drop(cut);
    let ((), calls, _) = measure_projection_work(|| {
        let mut fresh = fixture.state.projection_cut(&fixture.metadata);
        assert!(fresh
            .scoped_grant(&fixture.people[0], fixture.proposal, None)
            .is_ok());
        let mut single = fixture.state.single_projection(&fixture.metadata);
        for _ in 0..2 {
            assert!(single
                .scoped_grant(&fixture.people[0], fixture.proposal, None)
                .is_ok());
        }
        assert!(single.owners.is_none());
    });
    assert_eq!(calls[5], 3); // New cut plus two independent one-off lookups.
    fixture.state.genesis = BlockHash::ZERO;
    let mut fresh = fixture.state.projection_cut(&fixture.metadata);
    assert_eq!(
        fresh
            .scoped_grant(&invalid, TxHash::ZERO, None)
            .err()
            .unwrap(),
        "invalid delegation identity"
    );
    assert!(fresh.owners.as_ref().unwrap().is_empty());
}

#[test]
fn membership_view_is_lazy_shared_and_bound_to_borrowed_electorate() {
    let fixture = Fixture::new();
    let smaller = fixture.people[..1].to_vec();
    let mut cut = fixture.state.projection_cut(&fixture.metadata);
    assert!(cut.eligible.is_none());
    assert!(cut.delegation(TxHash::ZERO, &fixture.people).is_err());
    assert!(
        cut.eligible.is_none(),
        "unknown proposal must precede population work"
    );
    let (_, calls, _) = measure_projection_work(|| {
        cut.delegation(fixture.proposal, &fixture.people).unwrap();
        let original = cut.eligible.as_ref().unwrap().1.clone();
        cut.delegation(fixture.proposal, &fixture.people).unwrap();
        assert!(std::sync::Arc::ptr_eq(
            &original,
            &cut.eligible.as_ref().unwrap().1
        ));
        cut.delegation(fixture.proposal, &smaller).unwrap();
        assert_eq!(cut.eligible.as_ref().unwrap().1.len(), 1);
        assert_eq!(original.len(), fixture.people.len());
        assert!(!std::sync::Arc::ptr_eq(
            &original,
            &cut.eligible.as_ref().unwrap().1
        ));
        cut.delegation(fixture.proposal, &fixture.people).unwrap();
    });
    assert_eq!(calls[7], 3);
}

#[test]
fn sparse_scope_index_matches_lazy_lookup_and_disables_on_corruption() {
    let mut fixture = Fixture::new();
    fixture.grant(0, 1, KeyScope::Parameter(GovernableParam::BrnRate));
    fixture.revoke(0, KeyScope::Proposal(fixture.proposal));
    {
        let expected = fixture
            .state
            .single_projection(&fixture.metadata)
            .delegation(fixture.proposal, &fixture.people)
            .unwrap();
        let mut cut = fixture.state.projection_cut(&fixture.metadata);
        assert!(cut.records.is_none());
        let actual = cut.delegation(fixture.proposal, &fixture.people).unwrap();
        assert!(matches!(cut.records, Some(Some(_))));
        assert_eq!(actual.save_state(), expected.save_state());
        assert_eq!(actual.key_ballot(&fixture.people[0]), Some(None));
    }
    let unrelated = scoped::key(
        fixture.state.network,
        fixture.state.genesis,
        &fixture.people[2],
        &KeyScope::Proposal(TxHash::new([89; 32])),
    )
    .unwrap();
    fixture.metadata.insert(unrelated.clone(), vec![0xff]);
    {
        let expected = fixture
            .state
            .single_projection(&fixture.metadata)
            .delegation(fixture.proposal, &fixture.people)
            .unwrap();
        let mut cut = fixture.state.projection_cut(&fixture.metadata);
        let actual = cut.delegation(fixture.proposal, &fixture.people).unwrap();
        assert!(
            matches!(cut.records, Some(None)),
            "unindexable records disable the entire shortcut"
        );
        assert_eq!(actual.save_state(), expected.save_state());
    }
    fixture.metadata.remove(&unrelated);
    let reached = scoped::key(
        fixture.state.network,
        fixture.state.genesis,
        &fixture.people[0],
        &KeyScope::Proposal(fixture.proposal),
    )
    .unwrap();
    fixture.metadata.insert(reached, vec![0xff]);
    let expected = fixture
        .state
        .single_projection(&fixture.metadata)
        .delegation(fixture.proposal, &fixture.people)
        .err()
        .unwrap();
    let mut cut = fixture.state.projection_cut(&fixture.metadata);
    assert_eq!(
        cut.delegation(fixture.proposal, &fixture.people)
            .err()
            .unwrap(),
        expected
    );
    assert!(matches!(cut.records, Some(None)));
}

#[test]
fn empty_scope_index_preserves_invalid_owner_errors() {
    let fixture = Fixture::new();
    let people = [WalletAddress::new("brst_invalid")];
    let expected = fixture
        .state
        .single_projection(&fixture.metadata)
        .delegation(fixture.proposal, &people)
        .err()
        .unwrap();
    let mut cut = fixture.state.projection_cut(&fixture.metadata);
    assert_eq!(
        cut.delegation(fixture.proposal, &people).err().unwrap(),
        expected
    );
    assert!(matches!(cut.records, Some(Some(_))));
    assert!(cut.owners.as_ref().unwrap().is_empty());
    assert_eq!(
        cut.scoped_grant(&people[0], TxHash::ZERO, None)
            .err()
            .unwrap(),
        "invalid delegation owner"
    );
    assert_eq!(
        cut.scoped_grant(&fixture.people[0], TxHash::ZERO, None)
            .err()
            .unwrap(),
        "zero delegation proposal"
    );
}
