//! Physical row differences within a tracked transition. Selection pools are
//! encoded only for targets whose selection map was touched.
use super::*;

fn header(
    engine: &VerificationOrchestrator,
    address: &WalletAddress,
) -> Result<Option<Header>, String> {
    let state = engine.states.get(address);
    if state.is_some_and(|s| s.target != *address)
        || engine
            .active_challenges
            .get(address)
            .is_some_and(|c| c.target != *address)
    {
        return Err("normalized target identity mismatch".into());
    }
    let selection = engine.selections.get(address);
    let prior = engine.prior_votes.get(address);
    let contexts = engine.attempts.votes.get(address);
    let mut counts = [0; DOMAINS];
    if let Some(state) = state {
        counts[..5].copy_from_slice(&[
            state.endorsements.len() as u64,
            state.endorser_set.len() as u64,
            state.selected_verifiers.len() as u64,
            state.votes.len() as u64,
            state.excluded_verifiers.len() as u64,
        ]);
    }
    counts[5] = prior.map_or(0, |v| v.len() as u64);
    counts[6] = selection.map_or(0, |s| s.request.storage_pool().len() as u64);
    counts[7] = contexts.map_or(0, |v| v.len() as u64);
    let target = Target {
        address: address.clone(),
        state: state.map(|s| VerificationState {
            target: s.target.clone(),
            phase: s.phase.clone(),
            endorsements: Vec::new().into(),
            endorser_set: HashSet::new().into(),
            selected_verifiers: Vec::new().into(),
            votes: Default::default(),
            revote_count: s.revote_count,
            excluded_verifiers: HashSet::new().into(),
            started_at: s.started_at,
        }),
        challenge: engine.active_challenges.get(address).cloned(),
        appeal: engine.active_appeals.get(address).cloned(),
        votes: prior.map(|_| PriorVotes(Vec::new())),
        selection: selection.map(|s| SelectionRound {
            request: s.request.storage_header(),
            certified: s.certified.clone(),
            context: Default::default(),
        }),
        vote_contexts: contexts.map(|_| BTreeMap::new()),
        challenge_context: engine.attempts.challenges.get(address).copied(),
    };
    Ok((target.state.is_some()
        || target.challenge.is_some()
        || target.appeal.is_some()
        || target.votes.is_some()
        || target.selection.is_some()
        || target.vote_contexts.is_some()
        || target.challenge_context.is_some())
    .then_some(Header { target, counts }))
}

fn rows(
    engine: &VerificationOrchestrator,
    address: &WalletAddress,
    mask: u8,
) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let prefix = root(address)?;
    let mut rows = BTreeMap::new();
    if let Some(header) = header(engine, address)? {
        put(&mut rows, format!("{prefix}0"), encode(&header)?)?;
    }
    let mut emit = |member: Member| -> Result<(), String> {
        #[cfg(test)]
        VISITS.with(|probe| {
            if let Some(mut counts) = probe.get() {
                counts[member.domain()] += 1;
                probe.set(Some(counts));
            }
        });
        put(
            &mut rows,
            format!("{prefix}1/{}", member.suffix()?),
            encode(&member)?,
        )
    };
    if let Some(state) = engine.states.get(address) {
        if mask & 1 != 0 {
            for v in &state.endorsements {
                emit(Member::Endorsement(v.clone()))?;
            }
        }
        if mask & 2 != 0 {
            for v in &state.endorser_set {
                emit(Member::Endorser(v.clone()))?;
            }
        }
        if mask & 4 != 0 {
            for (i, v) in state.selected_verifiers.iter().enumerate() {
                emit(Member::Selected(i as u64, v.clone()))?;
            }
        }
        if mask & 8 != 0 {
            for v in &state.votes {
                emit(Member::Vote(v.clone()))?;
            }
        }
        if mask & 16 != 0 {
            for v in &state.excluded_verifiers {
                emit(Member::Excluded(v.clone()))?;
            }
        }
    }
    if mask & 32 != 0 {
        if let Some(votes) = engine.prior_votes.get(address) {
            for v in votes {
                emit(Member::PriorVote(v.clone()))?;
            }
        }
    }
    if mask & 64 != 0 {
        if let Some(selection) = engine.selections.get(address) {
            for (i, v) in selection.request.storage_pool().iter().enumerate() {
                emit(Member::Pool(i as u64, v.clone()))?;
            }
        }
    }
    if mask & 128 != 0 {
        if let Some(contexts) = engine.attempts.votes.get(address) {
            for (v, hash) in contexts {
                emit(Member::Context(v.clone(), *hash))?;
            }
        }
    }
    Ok(rows)
}

/// Visit exact changed physical target rows for a tracked transition. Unchanged
/// shared collections are skipped. Changed vector/set domains are re-encoded;
/// current votes and attempt contexts use persistent-map differences instead.
/// The writer must enforce aggregate budgets, current before-images and atomicity.
/// A callback error must abort its transaction; no partial publication is authorized.
pub fn visit_target_delta(
    before: &VerificationOrchestrator,
    after: &VerificationOrchestrator,
    mut visit: impl FnMut(String, Option<Vec<u8>>, Option<Vec<u8>>) -> Result<(), String>,
) -> Result<(), String> {
    if !after.record_scope_matches(before) {
        return Err("normalized target scope differs from baseline".into());
    }
    let mut targets = BTreeMap::<WalletAddress, u8>::new();
    let mut mark = |address: &WalletAddress, mask: u8| {
        *targets.entry(address.clone()).or_default() |= mask;
    };
    for a in after.states.changed_keys() {
        mark(a, 31);
    }
    for a in after.prior_votes.changed_keys() {
        mark(a, 32);
    }
    for a in after.selections.changed_keys() {
        mark(a, 64);
    }
    for a in after.attempts.votes.changed_keys() {
        mark(a, 128);
    }
    for a in after
        .active_challenges
        .changed_keys()
        .chain(after.active_appeals.changed_keys())
        .chain(after.attempts.challenges.changed_keys())
    {
        mark(a, 0);
    }
    for (address, mut mask) in targets {
        if let (Some(old), Some(new)) = (before.states.get(&address), after.states.get(&address)) {
            if old.endorsements.shares_storage_with(&new.endorsements) {
                mask &= !1;
            }
            if old.endorser_set.shares_storage_with(&new.endorser_set) {
                mask &= !2;
            }
            if old
                .selected_verifiers
                .shares_storage_with(&new.selected_verifiers)
            {
                mask &= !4;
            }
            if old
                .excluded_verifiers
                .shares_storage_with(&new.excluded_verifiers)
            {
                mask &= !16;
            }
        }
        let mut old = rows(before, &address, mask & !(128 | 8))?;
        let new = rows(after, &address, mask & !(128 | 8))?;
        let mut changes = BTreeMap::new();
        for (key, value) in new {
            let previous = old.remove(&key);
            if previous.as_ref() != Some(&value) {
                changes.insert(key, (previous, Some(value)));
            }
        }
        for (key, previous) in old {
            changes.insert(key, (Some(previous), None));
        }
        if mask & 8 != 0 {
            use imbl::ordmap::DiffItem;
            let empty = imbl::OrdMap::new();
            let old = before
                .states
                .get(&address)
                .map(|s| s.votes.indexed())
                .transpose()?
                .unwrap_or(&empty);
            let new = after
                .states
                .get(&address)
                .map(|s| s.votes.indexed())
                .transpose()?
                .unwrap_or(&empty);
            let prefix = root(&address)?;
            for change in old.diff(new) {
                let (previous, value) = match change {
                    DiffItem::Add(_, value) => (None, Some(value)),
                    DiffItem::Remove(_, value) => (Some(value), None),
                    DiffItem::Update {
                        old: (_, previous),
                        new: (_, value),
                    } => (Some(previous), Some(value)),
                };
                let key = format!(
                    "{prefix}1/{}",
                    Member::Vote(value.or(previous).unwrap().clone()).suffix()?
                );
                if key.len() > 511 {
                    return Err("normalized verification key exceeds LMDB limit".into());
                }
                let encode_vote = |vote: &VerifierVote| {
                    #[cfg(test)]
                    VISITS.with(|probe| {
                        if let Some(mut counts) = probe.get() {
                            counts[3] += 1;
                            probe.set(Some(counts));
                        }
                    });
                    encode(&Member::Vote(vote.clone()))
                };
                changes.insert(
                    key,
                    (
                        previous.map(encode_vote).transpose()?,
                        value.map(encode_vote).transpose()?,
                    ),
                );
            }
        }
        if mask & 128 != 0 {
            use imbl::ordmap::DiffItem;
            let empty = imbl::OrdMap::new();
            let old = before.attempts.votes.get(&address).unwrap_or(&empty);
            let new = after.attempts.votes.get(&address).unwrap_or(&empty);
            let prefix = root(&address)?;
            for change in old.diff(new) {
                let (voter, previous, value) = match change {
                    DiffItem::Add(voter, value) => (voter, None, Some(value)),
                    DiffItem::Remove(voter, value) => (voter, Some(value), None),
                    DiffItem::Update {
                        old: (_, previous),
                        new: (voter, value),
                    } => (voter, Some(previous), Some(value)),
                };
                let key = format!(
                    "{prefix}1/{}",
                    Member::Context(voter.clone(), BlockHash::ZERO).suffix()?
                );
                if key.len() > 511 {
                    return Err("normalized verification key exceeds LMDB limit".into());
                }
                let encode_context = |value: &BlockHash| {
                    #[cfg(test)]
                    VISITS.with(|probe| {
                        if let Some(mut counts) = probe.get() {
                            counts[7] += 1;
                            probe.set(Some(counts));
                        }
                    });
                    encode(&Member::Context(voter.clone(), *value))
                };
                changes.insert(
                    key,
                    (
                        previous.map(encode_context).transpose()?,
                        value.map(encode_context).transpose()?,
                    ),
                );
            }
        }
        for (key, (previous, value)) in changes {
            visit(key, previous, value)?;
        }
    }
    Ok(())
}

/// Visit all changed physical rows without constructing aggregate target records.
/// Rules changes may require the writer's global undo boundary.
pub fn visit_state_delta(
    before: &VerificationOrchestrator,
    after: &VerificationOrchestrator,
    mut visit: impl FnMut(String, Option<Vec<u8>>, Option<Vec<u8>>) -> Result<(), String>,
) -> Result<(), String> {
    if !after.record_scope_matches(before) {
        return Err("normalized state scope differs from baseline".into());
    }
    let (key, old) = before.rules_record().storage_entry()?;
    let (_, new) = after.rules_record().storage_entry()?;
    if old != new {
        visit(key, Some(old), Some(new))?;
    }
    let verifiers: BTreeSet<_> = after
        .penalized_verifiers
        .changed_keys()
        .map(WalletAddress::as_str)
        .chain(
            after
                .neither_tracker
                .history
                .changed_keys()
                .map(String::as_str),
        )
        .collect();
    for address in verifiers {
        let address = WalletAddress::new(address);
        let old = before
            .verifier_record(&address)
            .map(|r| r.storage_entry())
            .transpose()?;
        let new = after
            .verifier_record(&address)
            .map(|r| r.storage_entry())
            .transpose()?;
        if old != new {
            visit(
                VerificationRecord::address_key(2, &address)?,
                old.map(|(_, bytes)| bytes),
                new.map(|(_, bytes)| bytes),
            )?;
        }
    }
    visit_target_delta(before, after, visit)
}

#[cfg(test)]
thread_local! {
    pub(crate) static VISITS: std::cell::Cell<Option<[usize; DOMAINS]>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vote_delta_skips_shared_committee_but_tracks_later_mutations() {
        for count in [128, 1024, 10_000] {
            let target = WalletAddress::new("brst_shared_target");
            let mut before = VerificationOrchestrator::new();
            let params = burst_types::ProtocolParams::default();
            before
                .process_endorsement_at(
                    &target,
                    &WalletAddress::new("brst_endorser"),
                    params.endorsement_burn_amount,
                    &params,
                    Timestamp::new(1),
                )
                .unwrap();
            let state = before.states.get_mut(&target).unwrap();
            state.selected_verifiers = (0..count)
                .map(|n| WalletAddress::new(format!("brst_panel_{n:06}")))
                .collect();
            for n in 1..count {
                state.votes.push(VerifierVote {
                    verifier: WalletAddress::new(format!("brst_panel_{n:06}")),
                    vote: crate::Vote::Legitimate,
                    timestamp: Timestamp::new(2),
                    stake_amount: 1,
                });
            }
            let mut after = before.storage_clone();
            after.begin_record_changes();
            after
                .states
                .get_mut(&target)
                .unwrap()
                .votes
                .push(VerifierVote {
                    verifier: WalletAddress::new("brst_panel_000000"),
                    vote: crate::Vote::Legitimate,
                    timestamp: Timestamp::new(2),
                    stake_amount: 1,
                });
            let mut patched = target_rows(&before.target_record(&target).unwrap()).unwrap();
            VISITS.with(|probe| probe.set(Some([0; DOMAINS])));
            visit_target_delta(&before, &after, |key, old, new| {
                assert_eq!(patched.get(&key), old.as_ref());
                match new {
                    Some(value) => {
                        patched.insert(key, value);
                    }
                    None => {
                        patched.remove(&key);
                    }
                }
                Ok(())
            })
            .unwrap();
            let visits = VISITS.with(|probe| probe.replace(None).unwrap());
            assert_eq!(visits, [0, 0, 0, 1, 0, 0, 0, 0]);
            assert_eq!(
                patched,
                target_rows(&after.target_record(&target).unwrap()).unwrap()
            );
            assert_eq!(before.states[&target].votes.len(), count - 1);
            let before = after.storage_clone();
            after.begin_record_changes();
            after
                .states
                .get_mut(&target)
                .unwrap()
                .selected_verifiers
                .clear();
            assert_eq!(before.states[&target].selected_verifiers.len(), count);
            visit_target_delta(&before, &after, |key, old, new| {
                assert_eq!(patched.get(&key), old.as_ref());
                match new {
                    Some(value) => {
                        patched.insert(key, value);
                    }
                    None => {
                        patched.remove(&key);
                    }
                }
                Ok(())
            })
            .unwrap();
            assert_eq!(
                patched,
                target_rows(&after.target_record(&target).unwrap()).unwrap()
            );
        }
    }

    #[test]
    fn context_delta_shares_history_and_only_encodes_changed_bindings() {
        for count in [128, 1024, 10_000] {
            let target = WalletAddress::new("brst_context_target");
            let context = BlockHash::new([1; 32]);
            let mut before = VerificationOrchestrator::new();
            for n in 0..count {
                before
                    .attempts
                    .bind_vote(
                        &target,
                        &WalletAddress::new(format!("brst_voter_{n:06}")),
                        context,
                    )
                    .unwrap();
            }
            let mut after = before.storage_clone();
            assert!(before
                .attempts
                .votes
                .get(&target)
                .unwrap()
                .ptr_eq(after.attempts.votes.get(&target).unwrap()));
            after.begin_record_changes();
            after
                .attempts
                .bind_vote(&target, &WalletAddress::new("brst_new_voter"), context)
                .unwrap();
            assert_eq!(before.attempts.votes.get(&target).unwrap().len(), count);
            let mut patched = target_rows(&before.target_record(&target).unwrap()).unwrap();
            VISITS.with(|probe| probe.set(Some([0; DOMAINS])));
            let mut changed = 0;
            visit_target_delta(&before, &after, |key, old, new| {
                assert_eq!(patched.get(&key), old.as_ref());
                match new {
                    Some(value) => {
                        patched.insert(key, value);
                    }
                    None => {
                        patched.remove(&key);
                    }
                }
                changed += 1;
                Ok(())
            })
            .unwrap();
            let visits = VISITS.with(|probe| probe.replace(None).unwrap());
            assert_eq!(
                visits[7], 1,
                "one context encoding regardless of retained history"
            );
            assert_eq!(changed, 2, "target count header and new binding");
            assert_eq!(
                patched,
                target_rows(&after.target_record(&target).unwrap()).unwrap()
            );
            // Clearing the attempt must still emit every required deletion.
            let before = after.storage_clone();
            after.begin_record_changes();
            after.attempts.clear(&target);
            visit_target_delta(&before, &after, |key, old, new| {
                assert_eq!(patched.get(&key), old.as_ref());
                assert!(new.is_none());
                patched.remove(&key);
                Ok(())
            })
            .unwrap();
            assert!(patched.is_empty());
        }
    }
}
