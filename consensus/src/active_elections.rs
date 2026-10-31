//! Active elections container — manages all ongoing consensus elections.
//!
//! This is the top-level coordinator for the ORV election system. It holds a
//! bounded set of active elections, routes incoming votes to the correct
//! election, and handles cleanup of expired elections.

mod dependency_changes;
use dependency_changes::DependencyChanges;

use crate::election::{Election, ElectionState, ElectionStatus};
use crate::error::ConsensusError;
use crate::vote_info::VoteResult;
use burst_types::{BlockHash, Timestamp, WalletAddress};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

#[derive(Clone)]
struct ConfirmationWake(Arc<dyn Fn() + Send + Sync>);
impl std::fmt::Debug for ConfirmationWake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConfirmationWake")
    }
}

#[derive(Clone, Debug)]
struct DependencyWait {
    candidate: BlockHash,
    hashes: BTreeSet<BlockHash>,
}

/// Container for all active consensus elections.
///
/// Elections are keyed by caller-supplied conflict roots. Election and candidate
/// counts are capped; these limits do not by themselves bound all vote storage.
#[derive(Clone, Debug)]
pub struct ActiveElections {
    /// Conflict root -> election.
    elections: HashMap<BlockHash, Election>,
    /// Validated candidate hash -> shared conflict root. Wire votes carry hashes,
    /// not roots; never let a peer choose the election a candidate belongs to.
    candidate_roots: HashMap<BlockHash, BlockHash>,
    candidates_by_root: HashMap<BlockHash, Vec<BlockHash>>,
    /// Maximum number of concurrent elections.
    max_elections: usize,
    early_votes: crate::VoteCache,
    /// Total frozen authority for new elections. Legacy component callers may
    /// supply a weight directly; the node sets the complete confirmed map.
    effective_weight: u128,
    authority: Option<Arc<HashMap<WalletAddress, u128>>>,
    electorates: HashMap<BlockHash, Arc<HashMap<WalletAddress, u128>>>,
    authority_epoch: Option<BlockHash>,
    quorum_bps: u16,
    publication_identity: Option<usize>,
    changed_roots: BTreeSet<BlockHash>,
    changed_cursor: Option<BlockHash>,
    repair_roots: BTreeSet<BlockHash>,
    handoff_roots: BTreeSet<BlockHash>,
    handoff_cursor: Option<BlockHash>,
    timeout_roots: BTreeSet<(u64, BlockHash)>,
    timeout_dirty: BTreeSet<BlockHash>,
    repair_cursor: Option<BlockHash>,
    request_cursor: Option<(BlockHash, usize)>,
    confirmation_wake: Option<ConfirmationWake>,
    waiting_dependencies: HashMap<BlockHash, DependencyWait>,
    dependent_roots: HashMap<BlockHash, BTreeSet<BlockHash>>,
    dependency_changes: DependencyChanges,
}

impl ActiveElections {
    pub fn new(max_elections: usize, online_weight: u128) -> Self {
        Self {
            elections: HashMap::new(),
            candidate_roots: HashMap::new(),
            candidates_by_root: HashMap::new(),
            max_elections,
            early_votes: crate::VoteCache::new(),
            effective_weight: online_weight,
            authority: None,
            electorates: HashMap::new(),
            authority_epoch: None,
            quorum_bps: 6700,
            publication_identity: None,
            changed_roots: BTreeSet::new(),
            changed_cursor: None,
            repair_roots: BTreeSet::new(),
            handoff_roots: BTreeSet::new(),
            handoff_cursor: None,
            timeout_roots: BTreeSet::new(),
            timeout_dirty: BTreeSet::new(),
            repair_cursor: None,
            request_cursor: None,
            confirmation_wake: None,
            waiting_dependencies: HashMap::new(),
            dependent_roots: HashMap::new(),
            dependency_changes: DependencyChanges::new(),
        }
    }

    /// The callback must be nonblocking and must not acquire election locks.
    pub fn set_confirmation_wake(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.confirmation_wake = Some(ConfirmationWake(wake));
        self.changed_roots.extend(self.elections.keys().copied());
        self.handoff_roots.extend(
            self.elections
                .iter()
                .filter_map(|(root, election)| election.is_confirmed().then_some(*root)),
        );
        self.wake_confirmation();
    }

    fn wake_confirmation(&self) {
        if !self.changed_roots.is_empty() {
            if let Some(wake) = &self.confirmation_wake {
                (wake.0)();
            }
        }
    }

    fn mark_changed(&mut self, root: BlockHash) {
        if self
            .elections
            .get(&root)
            .is_some_and(Election::is_confirmed)
        {
            self.mark_handoff_candidate(root);
        }
        if self.changed_roots.insert(root) {
            self.wake_confirmation();
        }
    }

    fn mark_handoff_candidate(&mut self, root: BlockHash) {
        if self.handoff_roots.insert(root) {
            if let Some(wake) = &self.confirmation_wake {
                (wake.0)();
            }
        }
    }

    /// Independent of vote scheduling. Failed/unavailable handoffs stay indexed
    /// until retirement; rotate so they cannot hide later confirmed elections.
    pub fn next_handoff_roots(&mut self, limit: usize) -> Vec<BlockHash> {
        use std::ops::Bound::{Excluded, Unbounded};
        let mut roots: Vec<_> = match self.handoff_cursor {
            Some(cursor) => self.handoff_roots.range((Excluded(cursor), Unbounded)),
            None => self.handoff_roots.range::<BlockHash, _>(..),
        }
        .take(limit)
        .copied()
        .collect();
        if roots.len() < limit {
            if let Some(cursor) = self.handoff_cursor {
                roots.extend(
                    self.handoff_roots
                        .range(..=cursor)
                        .take(limit - roots.len())
                        .copied(),
                );
            }
        }
        if let Some(last) = roots.last() {
            self.handoff_cursor = Some(*last);
        }
        roots.retain(|root| {
            if self.elections.get(root).is_some_and(Election::is_confirmed) {
                true
            } else {
                self.handoff_roots.remove(root);
                false
            }
        });
        roots
    }

    /// Capture before checking durable dependencies; registration detects a
    /// relevant cementation racing that check. Evicted race evidence forces a retry.
    pub fn dependency_generation(&self) -> u64 {
        self.dependency_changes.generation()
    }

    pub fn clear_dependency_wait(&mut self, root: &BlockHash) {
        if let Some(dependencies) = self.waiting_dependencies.remove(root) {
            for dependency in dependencies.hashes {
                if let Some(roots) = self.dependent_roots.get_mut(&dependency) {
                    roots.remove(root);
                    if roots.is_empty() {
                        self.dependent_roots.remove(&dependency);
                    }
                }
            }
        }
    }

    /// These dependencies schedule a retry only; they do not authorize a vote.
    pub fn wait_for_dependencies(
        &mut self,
        root: BlockHash,
        candidate: BlockHash,
        dependencies: impl IntoIterator<Item = BlockHash>,
        generation: u64,
    ) {
        self.clear_dependency_wait(&root);
        if !self.elections.contains_key(&root) {
            return;
        }
        let dependencies: BTreeSet<_> = dependencies.into_iter().collect();
        if dependencies.is_empty()
            || self
                .dependency_changes
                .changed_since(generation, &dependencies)
        {
            self.mark_changed(root);
            return;
        }
        for dependency in &dependencies {
            self.dependent_roots
                .entry(*dependency)
                .or_default()
                .insert(root);
        }
        self.waiting_dependencies.insert(
            root,
            DependencyWait {
                candidate,
                hashes: dependencies,
            },
        );
    }

    /// Apply a bounded prefix under one election writer. Charge both removal of
    /// old edges and insertion of new ones. An oversized single root retains the
    /// scalar operation's behavior, but cannot bring another root into its batch.
    pub fn wait_for_dependencies_batch(
        &mut self,
        waits: &[(BlockHash, BlockHash, Vec<BlockHash>)],
        generation: u64,
    ) -> usize {
        let mut applied = 0;
        let mut edges = 0usize;
        for (root, candidate, dependencies) in waits.iter().take(32) {
            let work = dependencies.len().saturating_add(
                self.waiting_dependencies
                    .get(root)
                    .map_or(0, |wait| wait.hashes.len()),
            );
            if applied > 0 && edges.saturating_add(work) > 1024 {
                break;
            }
            self.wait_for_dependencies(*root, *candidate, dependencies.iter().copied(), generation);
            applied += 1;
            edges = edges.saturating_add(work);
            if edges >= 1024 {
                break;
            }
        }
        applied
    }

    /// Call only after durable cementation. Visit affected waiters, not every
    /// active election; each retry rechecks the complete causal dependency set.
    pub fn dependencies_cemented(&mut self, hashes: &[BlockHash]) {
        if hashes.is_empty() {
            return;
        }
        self.dependency_changes.record(hashes);
        let mut ready = BTreeSet::new();
        for hash in hashes {
            if let Some(roots) = self.dependent_roots.get(hash) {
                ready.extend(roots.iter().copied());
            }
        }
        for root in ready {
            self.clear_dependency_wait(&root);
            if self.elections.contains_key(&root) {
                self.mark_changed(root);
            }
        }
    }

    /// Coalesce vote changes per root; a repair pass revisits retained failures.
    pub fn confirmation_roots(&mut self, repair: bool, limit: usize) -> Vec<BlockHash> {
        if repair {
            use std::ops::Bound::{Excluded, Unbounded};
            let mut roots: Vec<_> = match self.repair_cursor {
                Some(cursor) => self.repair_roots.range((Excluded(cursor), Unbounded)),
                None => self.repair_roots.range::<BlockHash, _>(..),
            }
            .take(limit)
            .copied()
            .collect();
            if roots.len() < limit {
                if let Some(cursor) = self.repair_cursor {
                    roots.extend(
                        self.repair_roots
                            .range(..=cursor)
                            .take(limit - roots.len())
                            .copied(),
                    );
                }
            }
            if let Some(last) = roots.last() {
                self.repair_cursor = Some(*last);
            }
            for root in &roots {
                self.changed_roots.remove(root);
            }
            self.wake_confirmation();
            return roots;
        }
        // Repeated changes to low hashes must not starve a higher dependency
        // waiter. Keep a cursor over the coalesced set, with no stale queue entries.
        use std::ops::Bound::{Excluded, Unbounded};
        let mut roots: Vec<_> = match self.changed_cursor {
            Some(cursor) => self.changed_roots.range((Excluded(cursor), Unbounded)),
            None => self.changed_roots.range::<BlockHash, _>(..),
        }
        .take(limit)
        .copied()
        .collect();
        if roots.len() < limit {
            if let Some(cursor) = self.changed_cursor {
                roots.extend(
                    self.changed_roots
                        .range(..=cursor)
                        .take(limit - roots.len())
                        .copied(),
                );
            }
        }
        if let Some(last) = roots.last() {
            self.changed_cursor = Some(*last);
        }
        for root in &roots {
            self.changed_roots.remove(root);
        }
        self.wake_confirmation();
        roots
    }

    /// Event-driven work skips a candidate already waiting for durable dependencies.
    /// Periodic repair bypasses the wait; a different leading fork is never hidden.
    pub fn soft_quorum_for_roots(
        &self,
        roots: &[BlockHash],
        our_rep: &WalletAddress,
        repair: bool,
    ) -> Vec<(BlockHash, BlockHash)> {
        roots
            .iter()
            .filter_map(|root| {
                let e = self.elections.get(root)?;
                if e.is_confirmed()
                    || e.is_expired()
                    || !e.has_quorum()
                    || e.last_votes.get(our_rep).is_some_and(|v| v.is_final)
                {
                    return None;
                }
                let (winner, _) = e.leading_block()?;
                if !repair
                    && self
                        .waiting_dependencies
                        .get(root)
                        .is_some_and(|wait| wait.candidate == winner)
                {
                    return None;
                }
                Some((*root, winner))
            })
            .collect()
    }

    pub fn confirmed_for_roots(&self, roots: &[BlockHash]) -> Vec<ElectionStatus> {
        roots
            .iter()
            .filter_map(|root| self.elections.get(root))
            .filter_map(Self::confirmed_status)
            .collect()
    }

    pub fn cleanup_confirmed_roots(
        &mut self,
        roots: &[BlockHash],
        processed: impl Fn(&BlockHash) -> bool,
    ) -> Vec<BlockHash> {
        let retired: Vec<_> = roots
            .iter()
            .filter(|root| {
                self.elections.get(root).is_some_and(|e| {
                    e.is_confirmed()
                        && e.leading_final_block()
                            .is_some_and(|(winner, _)| processed(&winner))
                })
            })
            .copied()
            .collect();
        for root in &retired {
            self.remove_root(root);
        }
        retired
    }

    /// Caller-supplied publication identity; authentication happens outside this container.
    pub fn publication_identity(&self) -> Option<usize> {
        self.publication_identity
    }

    pub fn mark_publication(&mut self, identity: Option<usize>) {
        self.publication_identity = identity;
    }

    /// Start a new election for the given conflict root.
    ///
    /// Returns an error at capacity; an existing root is left unchanged.
    pub fn start_election(
        &mut self,
        root: BlockHash,
        now: Timestamp,
    ) -> Result<(), ConsensusError> {
        if self.elections.contains_key(&root) {
            return Ok(());
        }
        if self.elections.len() >= self.max_elections {
            return Err(ConsensusError::ElectionCapacityReached(self.max_elections));
        }

        let election = Election::with_quorum(root, self.effective_weight, now, self.quorum_bps);
        if let Some(authority) = &self.authority {
            self.electorates.insert(root, Arc::clone(authority));
        }
        self.elections.insert(root, election);
        self.repair_roots.insert(root);
        self.timeout_roots.insert((now.as_secs(), root));
        Ok(())
    }

    /// Register a locally validated candidate before accepting votes for it.
    /// Competing candidates must use the same account-position root.
    pub fn register_candidate(
        &mut self,
        root: BlockHash,
        candidate: BlockHash,
        now: Timestamp,
    ) -> Result<(), ConsensusError> {
        if let Some(existing) = self.candidate_roots.get(&candidate) {
            return if *existing == root {
                Ok(())
            } else {
                Err(ConsensusError::Other(
                    "candidate already belongs to another election".into(),
                ))
            };
        }
        // Bound candidate routing independently of the number of elections.
        if self.candidates_by_root.get(&root).map_or(0, Vec::len) >= 16 {
            return Err(ConsensusError::Other(
                "election candidate limit reached".into(),
            ));
        }
        self.start_election(root, now)?;
        self.candidate_roots.insert(candidate, root);
        self.candidates_by_root
            .entry(root)
            .or_default()
            .push(candidate);
        for vote in self.early_votes.drain_signed(&candidate) {
            // Replay uses the frozen electorate and normal final/sequence guards.
            // Conflicts or votes after quorum are rejected just as on live arrival.
            let _ = self.process_signed_vote(&root, vote, now);
        }
        Ok(())
    }

    pub fn candidate_root(&self, candidate: &BlockHash) -> Option<BlockHash> {
        self.candidate_roots.get(candidate).copied()
    }

    /// Rotate through roots and their bounded candidate lists without collecting
    /// the whole routing table. Empty/finished roots consume scan budget too.
    pub fn next_unconfirmed_candidates(
        &mut self,
        limit: usize,
        scan_limit: usize,
    ) -> Vec<BlockHash> {
        use std::ops::Bound::{Excluded, Unbounded};
        let mut hashes = Vec::new();
        let mut first_position = None;
        let mut previous_position = None;
        let mut wrapped = false;
        for _ in 0..scan_limit {
            if hashes.len() == limit {
                break;
            }
            let position = self
                .request_cursor
                .and_then(|(root, index)| {
                    let candidates = self.candidates_by_root.get(&root)?;
                    index
                        .checked_add(1)
                        .filter(|&next| next < candidates.len())
                        .map(|next| (root, next))
                })
                .or_else(|| {
                    let next = self.request_cursor.and_then(|(root, _)| {
                        self.repair_roots.range((Excluded(root), Unbounded)).next()
                    });
                    next.or_else(|| self.repair_roots.first())
                        .map(|&root| (root, 0))
                });
            let Some((root, index)) = position else {
                break;
            };
            if previous_position.is_some_and(|previous| (root, index) <= previous) {
                wrapped = true;
            }
            if wrapped && first_position.is_some_and(|first| (root, index) >= first) {
                break;
            }
            first_position.get_or_insert((root, index));
            previous_position = position;
            let election = &self.elections[&root];
            let candidate = self
                .candidates_by_root
                .get(&root)
                .and_then(|candidates| candidates.get(index));
            if !election.is_confirmed() && !election.is_expired() {
                if let Some(&candidate) = candidate {
                    hashes.push(candidate);
                    self.request_cursor = Some((root, index));
                    continue;
                }
            }
            // Skip the remaining candidates of an inactive or empty root.
            self.request_cursor = Some((root, usize::MAX));
        }
        hashes
    }

    /// Apply a local vote with a synthesized sequence number.
    ///
    /// Unlike `process_signed_vote`, this does not require candidate registration.
    /// A frozen roster overrides the supplied weight. Returns a status if confirmed.
    pub fn process_vote(
        &mut self,
        root: &BlockHash,
        voter: &WalletAddress,
        block: BlockHash,
        weight: u128,
        is_final: bool,
        now: Timestamp,
    ) -> Result<Option<ElectionStatus>, ConsensusError> {
        let weight = self
            .electorates
            .get(root)
            .map_or(weight, |roster| roster.get(voter).copied().unwrap_or(0));
        let election = self
            .elections
            .get_mut(root)
            .ok_or_else(|| ConsensusError::ElectionNotFound(format!("{}", root)))?;

        if election.is_confirmed() {
            return Err(ConsensusError::ElectionAlreadyConfirmed);
        }

        let vote_result = election.vote(voter, block, weight, is_final, now);

        match vote_result {
            VoteResult::Error(msg) => Err(ConsensusError::FinalVoteAlreadyCast(msg)),
            _ => {
                let status = election.try_confirm(now);
                if vote_result != VoteResult::Ignored {
                    self.mark_changed(*root);
                }
                Ok(status)
            }
        }
    }

    /// A current-state no-op proof for authenticated arrivals. Hold the read
    /// guard through the decision; unknown/retired candidates still need routing
    /// so early votes survive registration races. No result is cached.
    pub fn signed_vote_has_no_effect(
        &self,
        candidate: &BlockHash,
        voter: &WalletAddress,
        is_final: bool,
        sequence: u64,
    ) -> bool {
        let Some(election) = self
            .candidate_root(candidate)
            .and_then(|root| self.elections.get(&root))
        else {
            return false;
        };
        if election.is_confirmed() || election.is_expired() {
            return true;
        }
        let Some(previous) = election.last_votes.get(voter) else {
            return false;
        };
        if !previous.is_final && (is_final || sequence > previous.sequence) {
            return false;
        }
        // Even an ignored vote runs try_confirm. Preserve that transition if a
        // caller has changed the threshold since the last vote was applied.
        !(election.confirmation_threshold() > 0
            && election
                .leading_final_block()
                .is_some_and(|(_, tally)| tally >= election.confirmation_threshold()))
    }

    /// Call only after signature verification. Sharing the election lock with
    /// registration makes an early arrival either cached or immediately routed.
    pub fn route_signed_vote(
        &mut self,
        mut vote: crate::VoteInfo,
        received_at: Timestamp,
    ) -> Result<Option<ElectionStatus>, ConsensusError> {
        if let Some(root) = self.candidate_root(&vote.block_hash) {
            return self.process_signed_vote(&root, vote, received_at);
        }
        if let Some(authority) = &self.authority {
            vote.weight = authority.get(&vote.voter).copied().unwrap_or(0);
        }
        self.early_votes.insert_signed(vote);
        Ok(None)
    }

    /// Process an authenticated wire vote using its signed ordering metadata.
    pub fn process_signed_vote(
        &mut self,
        root: &BlockHash,
        mut vote: crate::vote_info::VoteInfo,
        received_at: Timestamp,
    ) -> Result<Option<ElectionStatus>, ConsensusError> {
        if self.candidate_root(&vote.block_hash) != Some(*root) {
            return Err(ConsensusError::Other("unregistered vote candidate".into()));
        }
        vote.weight = self.electorates.get(root).map_or(vote.weight, |roster| {
            roster.get(&vote.voter).copied().unwrap_or(0)
        });
        let election = self
            .elections
            .get_mut(root)
            .ok_or_else(|| ConsensusError::ElectionNotFound(root.to_string()))?;
        let result = election.vote_signed(vote, received_at);
        match result {
            VoteResult::Error(message) => Err(ConsensusError::FinalVoteAlreadyCast(message)),
            _ => {
                let status = election.try_confirm(received_at);
                if result != VoteResult::Ignored {
                    self.mark_changed(*root);
                }
                Ok(status)
            }
        }
    }

    /// Collect `(root, winner)` pairs with a soft quorum and no final vote from
    /// `our_rep`. The caller still checks durable dependencies and voting authority
    /// before signing. This full scan does not consult dependency waiters.
    pub fn soft_quorum_needing_final(
        &self,
        our_rep: &WalletAddress,
    ) -> Vec<(BlockHash, BlockHash)> {
        self.elections
            .iter()
            .filter(|(_, e)| !e.is_confirmed() && !e.is_expired() && e.has_quorum())
            .filter(|(_, e)| e.last_votes.get(our_rep).map(|v| v.is_final) != Some(true))
            .filter_map(|(root, e)| e.leading_block().map(|(winner, _)| (*root, winner)))
            .collect()
    }

    /// Expire and remove nonterminal elections whose age reaches the timeout.
    /// Returns the roots transitioned by this call. Already-terminal elections
    /// are skipped; explicit expiration requires separate retirement.
    pub fn cleanup_expired(&mut self, timeout_ms: u64, now: Timestamp) -> Vec<BlockHash> {
        // Mutable election access can change creation time or revive a terminal
        // state. Reindex only those roots before selecting the due prefix.
        for root in std::mem::take(&mut self.timeout_dirty) {
            if let Some(election) = self.elections.get(&root) {
                self.timeout_roots
                    .insert((election.created_at.as_secs(), root));
            }
        }
        let mut expired = Vec::new();
        while let Some(&(created, root)) = self.timeout_roots.first() {
            if now.as_secs().saturating_sub(created).saturating_mul(1000) < timeout_ms {
                break;
            }
            self.timeout_roots.pop_first();
            if self
                .elections
                .get_mut(&root)
                .is_some_and(|election| election.check_timeout(timeout_ms, now))
            {
                expired.push(root);
                self.remove_root(&root);
            }
        }

        expired
    }

    /// Remove confirmed elections after their results have been processed.
    ///
    /// The caller must finish ledger processing first. This unconditional cleanup
    /// does not check whether winners were durably cemented.
    pub fn cleanup_confirmed(&mut self) -> Vec<BlockHash> {
        self.cleanup_confirmed_where(|_| true)
    }

    /// Retire confirmed elections whose winners satisfy the caller's `processed` predicate.
    pub fn cleanup_confirmed_where(
        &mut self,
        processed: impl Fn(&BlockHash) -> bool,
    ) -> Vec<BlockHash> {
        let confirmed: Vec<BlockHash> = self
            .elections
            .iter()
            .filter(|(_, e)| e.state == ElectionState::Confirmed)
            .filter(|(_, e)| {
                e.leading_final_block()
                    .is_some_and(|(winner, _)| processed(&winner))
            })
            .map(|(root, _)| *root)
            .collect();

        for root in &confirmed {
            self.remove_root(root);
        }

        confirmed
    }

    /// Get a reference to an election by its root.
    pub fn get_election(&self, root: &BlockHash) -> Option<&Election> {
        self.elections.get(root)
    }

    /// Retire an election whose account position was removed by ledger replacement.
    pub fn remove_root(&mut self, root: &BlockHash) {
        self.clear_dependency_wait(root);
        self.changed_roots.remove(root);
        if let Some(election) = self.elections.remove(root) {
            self.timeout_roots
                .remove(&(election.created_at.as_secs(), *root));
        }
        self.timeout_dirty.remove(root);
        self.repair_roots.remove(root);
        self.handoff_roots.remove(root);
        if let Some(candidates) = self.candidates_by_root.remove(root) {
            for candidate in candidates {
                self.candidate_roots.remove(&candidate);
            }
        }
        self.electorates.remove(root);
    }

    /// Get a mutable reference to an election by its root.
    pub fn get_election_mut(&mut self, root: &BlockHash) -> Option<&mut Election> {
        if let Some(election) = self.elections.get(root) {
            self.timeout_roots
                .remove(&(election.created_at.as_secs(), *root));
            self.timeout_dirty.insert(*root);
            // The caller may confirm or reopen it after this borrow returns.
            self.mark_handoff_candidate(*root);
            self.mark_changed(*root);
        }
        self.elections.get_mut(root)
    }

    /// Number of retained elections, including terminal ones awaiting retirement.
    pub fn election_count(&self) -> usize {
        self.elections.len()
    }

    /// Collect status for all confirmed elections.
    pub fn confirmed_elections(&self) -> Vec<ElectionStatus> {
        self.elections
            .values()
            .filter_map(Self::confirmed_status)
            .collect()
    }

    fn confirmed_status(e: &Election) -> Option<ElectionStatus> {
        if !e.is_confirmed() {
            return None;
        }
        let (winner, final_tally) = e.leading_final_block()?;
        Some(ElectionStatus {
            winner,
            tally: e.tally.get(&winner).copied().unwrap_or(final_tally),
            final_tally,
            election_duration_ms: e
                .state_changed_at
                .as_secs()
                .saturating_sub(e.created_at.as_secs())
                .saturating_mul(1000),
        })
    }

    /// Legacy component configuration. Once an authority map is installed,
    /// attendance samples cannot change the denominator for new elections.
    pub fn set_online_weight(&mut self, weight: u128) {
        self.publication_identity = None;
        if self.authority.is_none() {
            self.effective_weight = weight;
        }
    }

    /// Freeze confirmed representative weights when each election starts.
    /// Online attendance and later delegation cannot rewrite an active tally.
    pub fn set_authority(
        &mut self,
        weights: HashMap<WalletAddress, u128>,
    ) -> Result<(), ConsensusError> {
        self.publication_identity = None;
        if self.authority_epoch.is_some() && self.authority.as_deref() != Some(&weights) {
            return Err(ConsensusError::Other(
                "certified authority requires an epoch handoff".into(),
            ));
        }
        let total = weights
            .values()
            .try_fold(0u128, |sum, weight| sum.checked_add(*weight))
            .ok_or_else(|| ConsensusError::Other("authority weight overflow".into()))?;
        self.authority = Some(Arc::new(weights));
        self.effective_weight = total;
        Ok(())
    }

    pub fn authority_epoch(&self) -> Option<BlockHash> {
        self.authority_epoch
    }

    /// The caller authenticates the installed checkpoint. A new epoch retires
    /// every old election and candidate route; attendance cannot reopen them.
    /// Repeating the same epoch requires identical weights and quorum. Structural
    /// checks enforce positive whole-person weights and a 6667–10000 bps quorum;
    /// they do not authenticate the checkpoint or establish epoch succession.
    pub fn install_authority(
        &mut self,
        epoch: BlockHash,
        weights: HashMap<WalletAddress, u128>,
        quorum_bps: u16,
    ) -> Result<(), ConsensusError> {
        self.publication_identity = None;
        let total = weights
            .values()
            .try_fold(0u128, |sum, weight| sum.checked_add(*weight))
            .ok_or_else(|| ConsensusError::Other("authority weight overflow".into()))?;
        if epoch.is_zero()
            || !(6667..=10000).contains(&quorum_bps)
            || total == 0
            || weights
                .values()
                .any(|weight| *weight == 0 || *weight % crate::rep_weights::WEIGHT_PER_HUMAN != 0)
        {
            return Err(ConsensusError::Other(
                "invalid certified payment authority".into(),
            ));
        }
        if self.authority_epoch == Some(epoch) {
            if self.authority.as_deref() != Some(&weights) || self.quorum_bps != quorum_bps {
                return Err(ConsensusError::Other(
                    "installed payment authority changed".into(),
                ));
            }
            return Ok(());
        }
        self.early_votes = crate::VoteCache::new();
        self.changed_roots.clear();
        self.changed_cursor = None;
        self.waiting_dependencies.clear();
        self.dependent_roots.clear();
        self.dependency_changes.reset();
        self.elections.clear();
        self.repair_roots.clear();
        self.handoff_roots.clear();
        self.handoff_cursor = None;
        self.timeout_roots.clear();
        self.timeout_dirty.clear();
        self.repair_cursor = None;
        self.request_cursor = None;
        self.candidate_roots.clear();
        self.candidates_by_root.clear();
        self.electorates.clear();
        self.authority = Some(Arc::new(weights));
        self.effective_weight = total;
        self.authority_epoch = Some(epoch);
        self.quorum_bps = quorum_bps;
        Ok(())
    }

    pub fn has_authorized_voter(&self, voter: &WalletAddress) -> bool {
        self.authority
            .as_ref()
            .is_some_and(|r| r.get(voter).is_some_and(|w| *w > 0))
            || self
                .electorates
                .values()
                .any(|r| r.get(voter).is_some_and(|w| *w > 0))
    }

    /// Whether the container has reached its capacity limit.
    pub fn is_at_capacity(&self) -> bool {
        self.elections.len() >= self.max_elections
    }

    /// Mark an unconfirmed election expired. The caller must retire it with
    /// `remove_root`; `cleanup_expired` only retires newly expired elections.
    pub fn expire_election(&mut self, root: BlockHash) -> Option<BlockHash> {
        if let Some(election) = self.elections.get_mut(&root) {
            if !election.is_confirmed() {
                election.state = ElectionState::Expired;
                return Some(root);
            }
        }
        None
    }

    /// Return nonwinning hashes present in the confirmed election's combined tally.
    /// Registered candidates without votes are absent. Ledger rollback is separate.
    pub fn get_fork_losers(&self, confirmed_root: &BlockHash) -> Vec<BlockHash> {
        if let Some(election) = self.elections.get(confirmed_root) {
            if election.is_confirmed() {
                // Exclude the final-tally winner, even if another candidate has
                // a larger combined tally. Election confirmation is not cementation.
                if let Some((winner, _)) = election.leading_final_block() {
                    return election
                        .tally
                        .keys()
                        .filter(|hash| **hash != winner)
                        .copied()
                        .collect();
                }
            }
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    // Component fixtures supply authority and vote metadata directly. Signature
    // verification, checkpoint authentication and ledger cementation are external.
    use super::*;

    #[test]
    fn read_only_vote_rejection_matches_routing_and_preserves_new_finals() {
        let root = BlockHash::new([211; 32]);
        let voter = WalletAddress::new("brst_read_only");
        let now = Timestamp::new(100);
        let state = |e: &Election| {
            bincode::serialize(&(
                e.id,
                &e.state,
                &e.last_votes,
                &e.tally,
                &e.final_tally,
                e.created_at,
                e.state_changed_at,
                e.confirmation_threshold(),
            ))
            .unwrap()
        };
        for prior_final in [false, true] {
            let mut active = ActiveElections::new(8, 1_000);
            active.register_candidate(root, root, now).unwrap();
            active
                .route_signed_vote(
                    crate::VoteInfo::new(voter.clone(), root, 100, prior_final, now, 10),
                    now,
                )
                .unwrap();
            for is_final in [false, true] {
                for sequence in [0, 9, 10, 11] {
                    let skip = active.signed_vote_has_no_effect(&root, &voter, is_final, sequence);
                    assert_eq!(skip, prior_final || (!is_final && sequence <= 10));
                    if !skip {
                        continue;
                    }
                    for weight in [0, 100] {
                        for received in [now, Timestamp::new(1_000)] {
                            let mut routed = active.clone();
                            let before = state(routed.get_election(&root).unwrap());
                            let result = routed.route_signed_vote(
                                crate::VoteInfo::new(
                                    voter.clone(),
                                    root,
                                    weight,
                                    is_final,
                                    now,
                                    sequence,
                                ),
                                received,
                            );
                            assert!(!matches!(result, Ok(Some(_))));
                            assert_eq!(before, state(routed.get_election(&root).unwrap()));
                            assert_eq!(active.changed_roots, routed.changed_roots);
                            assert_eq!(active.handoff_roots, routed.handoff_roots);
                        }
                    }
                }
            }
            assert!(!active.signed_vote_has_no_effect(
                &root,
                &WalletAddress::new("brst_new"),
                true,
                10
            ));
            active.remove_root(&root);
            assert!(!active.signed_vote_has_no_effect(&root, &voter, true, 10));
        }
    }

    #[test]
    fn read_only_vote_rejection_rechecks_confirmation_and_terminal_state() {
        let root = BlockHash::new([212; 32]);
        let voter = WalletAddress::new("brst_read_only");
        let now = Timestamp::new(100);
        let mut active = ActiveElections::new(8, 1_000);
        active.register_candidate(root, root, now).unwrap();
        active
            .route_signed_vote(
                crate::VoteInfo::new(voter.clone(), root, 100, true, now, 10),
                now,
            )
            .unwrap();
        // Model a newly reachable quorum: ignored sequence zero must still be
        // allowed through try_confirm, rather than a cached rejection.
        active
            .elections
            .get_mut(&root)
            .unwrap()
            .final_tally
            .insert(root, 1_000);
        assert!(!active.signed_vote_has_no_effect(&root, &voter, true, 0));
        assert!(active
            .route_signed_vote(
                crate::VoteInfo::new(voter.clone(), root, 100, true, now, 0),
                now
            )
            .unwrap()
            .is_some());
        assert!(active.signed_vote_has_no_effect(&root, &voter, false, 11));
        active.elections.get_mut(&root).unwrap().state = ElectionState::Expired;
        assert!(active.signed_vote_has_no_effect(&root, &voter, true, 12));
    }

    #[test]
    fn authenticated_early_votes_confirm_only_after_validated_registration() {
        let mut elections = ActiveElections::new(8, 100);
        let root = BlockHash::new([201; 32]);
        let hash = BlockHash::new([202; 32]);
        let voter = WalletAddress::new("brst_early".to_owned());
        elections
            .set_authority(HashMap::from([(voter.clone(), 100)]))
            .unwrap();
        let vote = crate::VoteInfo::new(voter.clone(), hash, 1, true, Timestamp::new(100), 77);
        elections
            .route_signed_vote(vote.clone(), Timestamp::new(100))
            .unwrap();
        assert!(elections.get_election(&root).is_none());
        assert_eq!(elections.early_votes.total_votes(), 1);
        elections
            .register_candidate(root, hash, Timestamp::new(101))
            .unwrap();
        let election = elections.get_election(&root).unwrap();
        assert!(election.is_confirmed());
        assert_eq!(election.last_votes[&voter].sequence, 77);
        assert_eq!(election.last_votes[&voter].weight, 100);
        assert!(elections.early_votes.is_empty());
        // The opposite arrival order uses exactly the live election guards.
        elections
            .route_signed_vote(vote, Timestamp::new(102))
            .unwrap();
        assert_eq!(
            elections.get_election(&root).unwrap().final_tally[&hash],
            100
        );
    }

    #[test]
    fn early_votes_use_registration_authority_and_do_not_cross_epoch_handoff() {
        let mut elections = ActiveElections::new(8, 100);
        let old = WalletAddress::new("brst_old".to_owned());
        let new = WalletAddress::new("brst_new".to_owned());
        let hash = BlockHash::new([203; 32]);
        let vote =
            |voter| crate::VoteInfo::new(voter, hash, u128::MAX, true, Timestamp::new(100), 1);
        elections
            .set_authority(HashMap::from([(old.clone(), 100)]))
            .unwrap();
        elections
            .route_signed_vote(vote(new.clone()), Timestamp::new(100))
            .unwrap();
        assert!(elections.early_votes.is_empty());
        elections
            .route_signed_vote(vote(old.clone()), Timestamp::new(100))
            .unwrap();
        elections
            .set_authority(HashMap::from([(new.clone(), 100)]))
            .unwrap();
        elections
            .register_candidate(hash, hash, Timestamp::new(101))
            .unwrap();
        assert!(!elections.get_election(&hash).unwrap().is_confirmed());
        assert!(elections.get_election(&hash).unwrap().last_votes.is_empty());
        let next = BlockHash::new([204; 32]);
        let mut early = vote(new.clone());
        early.block_hash = next;
        elections
            .route_signed_vote(early, Timestamp::new(101))
            .unwrap();
        elections
            .install_authority(
                BlockHash::new([205; 32]),
                HashMap::from([(new, 10000)]),
                6700,
            )
            .unwrap();
        assert!(elections.early_votes.is_empty());
        elections
            .register_candidate(next, next, Timestamp::new(102))
            .unwrap();
        assert!(!elections.get_election(&next).unwrap().is_confirmed());
    }

    #[test]
    fn certified_epoch_retires_old_routes_and_freezes_its_quorum() {
        let old = WalletAddress::new("brst_old");
        let next = WalletAddress::new("brst_next");
        let mut elections = ActiveElections::new(10, 0);
        let first = BlockHash::new([1; 32]);
        let second = BlockHash::new([2; 32]);
        let root = BlockHash::new([3; 32]);
        let candidate = BlockHash::new([4; 32]);
        elections
            .install_authority(first, HashMap::from([(old.clone(), 100_000)]), 6700)
            .unwrap();
        elections
            .register_candidate(root, candidate, Timestamp::new(1))
            .unwrap();
        assert_eq!(
            elections
                .get_election(&root)
                .unwrap()
                .confirmation_threshold(),
            67_000
        );
        elections
            .install_authority(second, HashMap::from([(next.clone(), 100_000)]), 8000)
            .unwrap();
        assert_eq!(elections.election_count(), 0);
        assert_eq!(elections.candidate_root(&candidate), None);
        assert!(!elections.has_authorized_voter(&old));
        assert!(elections.has_authorized_voter(&next));
        elections.set_online_weight(1);
        elections
            .register_candidate(root, BlockHash::new([5; 32]), Timestamp::new(2))
            .unwrap();
        assert_eq!(
            elections
                .get_election(&root)
                .unwrap()
                .confirmation_threshold(),
            80_000
        );
        elections
            .install_authority(second, HashMap::from([(next.clone(), 100_000)]), 8000)
            .unwrap();
        assert_eq!(elections.election_count(), 1);
        assert!(elections
            .install_authority(second, HashMap::from([(next, 100_000)]), 6700)
            .is_err());
        assert!(elections
            .set_authority(HashMap::from([(old, 100_000)]))
            .is_err());
        assert_eq!(
            elections
                .get_election(&root)
                .unwrap()
                .confirmation_threshold(),
            80_000
        );
    }

    #[test]
    fn invalid_certified_authority_preserves_current_elections() {
        let voter = WalletAddress::new("brst_human");
        let mut elections = ActiveElections::new(10, 0);
        let epoch = BlockHash::new([1; 32]);
        elections
            .install_authority(epoch, HashMap::from([(voter.clone(), 10_000)]), 6700)
            .unwrap();
        elections
            .register_candidate(epoch, epoch, Timestamp::new(1))
            .unwrap();
        for (next, weight, quorum) in [
            (BlockHash::ZERO, 10_000, 6700),
            (BlockHash::new([2; 32]), 1, 6700),
            (BlockHash::new([2; 32]), 10_000, 6666),
            (BlockHash::new([2; 32]), 0, 6700),
        ] {
            assert!(elections
                .install_authority(next, HashMap::from([(voter.clone(), weight)]), quorum)
                .is_err());
            assert_eq!(elections.authority_epoch(), Some(epoch));
            assert_eq!(elections.candidate_root(&epoch), Some(epoch));
        }
    }

    fn make_hash(byte: u8) -> BlockHash {
        BlockHash::new([byte; 32])
    }

    fn make_voter(name: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{name}"))
    }

    fn ts(secs: u64) -> Timestamp {
        Timestamp::new(secs)
    }

    #[test]
    fn start_election() {
        let mut elections = ActiveElections::new(10, 1000);
        assert!(elections.start_election(make_hash(1), ts(100)).is_ok());
        assert_eq!(elections.election_count(), 1);
        assert!(elections.get_election(&make_hash(1)).is_some());
    }

    #[test]
    fn frozen_authority_ignores_claimed_weights_attendance_and_later_delegation() {
        let alice = make_voter("alice");
        let bob = make_voter("bob");
        let carol = make_voter("carol");
        let mallory = make_voter("mallory");
        let mut elections = ActiveElections::new(10, 0);
        elections
            .set_authority(HashMap::from([
                (alice.clone(), 10_000),
                (bob.clone(), 10_000),
                (carol.clone(), 10_000),
            ]))
            .unwrap();
        let root = make_hash(1);
        let block = make_hash(2);
        elections.register_candidate(root, block, ts(100)).unwrap();
        elections.set_online_weight(1);
        elections
            .set_authority(HashMap::from([(mallory.clone(), 30_000)]))
            .unwrap();
        assert!(elections
            .process_vote(&root, &mallory, block, u128::MAX, true, ts(101))
            .unwrap()
            .is_none());
        assert!(elections
            .process_vote(&root, &alice, block, u128::MAX, true, ts(102))
            .unwrap()
            .is_none());
        assert!(elections
            .process_vote(&root, &bob, block, 0, true, ts(102))
            .unwrap()
            .is_none());
        assert_eq!(
            elections.get_election(&root).unwrap().final_tally[&block],
            20_000
        );
        assert_eq!(
            elections
                .get_election(&root)
                .unwrap()
                .confirmation_threshold(),
            20_100
        );
        assert!(elections
            .process_vote(&root, &carol, block, 0, true, ts(102))
            .unwrap()
            .is_some());
        let next = make_hash(3);
        elections
            .register_candidate(next, make_hash(4), ts(103))
            .unwrap();
        assert_eq!(
            elections
                .get_election(&next)
                .unwrap()
                .confirmation_threshold(),
            20_100
        );
        assert!(elections
            .process_vote(&next, &alice, make_hash(4), u128::MAX, true, ts(104))
            .unwrap()
            .is_none());
        assert!(elections
            .process_vote(&next, &mallory, make_hash(4), 0, true, ts(104))
            .unwrap()
            .is_some());
        elections.cleanup_confirmed();
        assert!(elections.electorates.is_empty());
    }

    #[test]
    fn empty_or_overflowing_authority_cannot_manufacture_a_quorum() {
        let mut elections = ActiveElections::new(10, 1);
        assert!(elections
            .set_authority(HashMap::from([
                (make_voter("a"), u128::MAX),
                (make_voter("b"), 1)
            ]))
            .is_err());
        elections.set_authority(HashMap::new()).unwrap();
        elections
            .register_candidate(make_hash(1), make_hash(2), ts(100))
            .unwrap();
        assert!(elections
            .process_vote(
                &make_hash(1),
                &make_voter("a"),
                make_hash(2),
                u128::MAX,
                true,
                ts(101)
            )
            .unwrap()
            .is_none());
    }

    #[test]
    fn duplicate_election_is_noop() {
        let mut elections = ActiveElections::new(10, 1000);
        elections.start_election(make_hash(1), ts(100)).unwrap();
        elections.start_election(make_hash(1), ts(101)).unwrap();
        assert_eq!(elections.election_count(), 1);
    }

    #[test]
    fn competing_candidates_share_one_election_even_at_capacity() {
        let mut elections = ActiveElections::new(1, 3);
        let root = make_hash(1);
        elections
            .register_candidate(root, make_hash(2), ts(100))
            .unwrap();
        elections
            .register_candidate(root, make_hash(3), ts(101))
            .unwrap();
        assert_eq!(elections.election_count(), 1);
        assert_eq!(elections.candidate_root(&make_hash(2)), Some(root));
        assert_eq!(elections.candidate_root(&make_hash(3)), Some(root));
        assert_eq!(elections.candidate_root(&make_hash(9)), None);
        assert!(elections
            .register_candidate(make_hash(4), make_hash(2), ts(101))
            .is_err());
        assert!(elections
            .register_candidate(make_hash(4), make_hash(5), ts(101))
            .is_err());
        assert_eq!(elections.candidate_root(&make_hash(5)), None);
        elections
            .process_vote(&root, &make_voter("alice"), make_hash(2), 1, true, ts(102))
            .unwrap();
        assert!(elections
            .process_vote(&root, &make_voter("alice"), make_hash(3), 1, true, ts(103))
            .is_err());
        assert_eq!(
            elections
                .get_election(&root)
                .unwrap()
                .final_tally
                .get(&make_hash(3)),
            None
        );
    }

    #[test]
    fn candidate_routes_are_bounded_and_removed_with_elections() {
        let mut elections = ActiveElections::new(1, 1);
        let root = make_hash(1);
        for n in 2..18 {
            elections
                .register_candidate(root, make_hash(n), ts(100))
                .unwrap();
        }
        assert!(elections
            .register_candidate(root, make_hash(18), ts(100))
            .is_err());
        elections
            .register_candidate(root, make_hash(2), ts(100))
            .unwrap();
        elections.cleanup_expired(1000, ts(102));
        assert!(elections.candidate_roots.is_empty());
        assert!(elections.candidates_by_root.is_empty());
        elections
            .register_candidate(root, make_hash(2), ts(103))
            .unwrap();
        elections
            .process_vote(&root, &make_voter("alice"), make_hash(2), 1, true, ts(104))
            .unwrap();
        elections.cleanup_confirmed();
        assert!(elections.candidate_roots.is_empty());
        assert!(elections.candidates_by_root.is_empty());
    }

    #[test]
    fn capacity_limit_enforced() {
        let mut elections = ActiveElections::new(2, 1000);
        elections.start_election(make_hash(1), ts(100)).unwrap();
        elections.start_election(make_hash(2), ts(101)).unwrap();

        let result = elections.start_election(make_hash(3), ts(102));
        assert!(result.is_err());
        match result.unwrap_err() {
            ConsensusError::ElectionCapacityReached(cap) => assert_eq!(cap, 2),
            e => panic!("unexpected error: {:?}", e),
        }
        assert!(elections.is_at_capacity());
    }

    #[test]
    fn process_vote_routes_to_election() {
        let mut elections = ActiveElections::new(10, 1000);
        elections.start_election(make_hash(1), ts(100)).unwrap();

        let result = elections.process_vote(
            &make_hash(1),
            &make_voter("alice"),
            make_hash(2),
            300,
            false,
            ts(101),
        );
        assert!(result.is_ok());
        assert!(result.unwrap().is_none()); // Not confirmed yet

        let election = elections.get_election(&make_hash(1)).unwrap();
        assert_eq!(election.tally.get(&make_hash(2)), Some(&300));
    }

    #[test]
    fn process_vote_election_not_found() {
        let mut elections = ActiveElections::new(10, 1000);
        let result = elections.process_vote(
            &make_hash(99),
            &make_voter("alice"),
            make_hash(2),
            100,
            false,
            ts(100),
        );
        assert!(matches!(result, Err(ConsensusError::ElectionNotFound(_))));
    }

    #[test]
    fn process_vote_confirms_election() {
        let mut elections = ActiveElections::new(10, 1000);
        elections.start_election(make_hash(1), ts(100)).unwrap();

        // Vote with enough weight to confirm (>= 670)
        let result = elections
            .process_vote(
                &make_hash(1),
                &make_voter("alice"),
                make_hash(2),
                700,
                true,
                ts(101),
            )
            .unwrap();

        assert!(result.is_some());
        let status = result.unwrap();
        assert_eq!(status.winner, make_hash(2));
        assert_eq!(status.tally, 700);
    }

    #[test]
    fn soft_quorum_needing_final_flags_then_clears() {
        let mut elections = ActiveElections::new(10, 1000);
        let root = make_hash(1);
        let block = make_hash(2);
        let our_rep = make_voter("self");
        elections.start_election(root, ts(100)).unwrap();

        // No votes → nothing at soft quorum.
        assert!(elections.soft_quorum_needing_final(&our_rep).is_empty());

        // A non-final supermajority (700 ≥ 670 margin over 0) → soft quorum,
        // and we haven't cast a final vote → flagged for us to finalize.
        elections
            .process_vote(&root, &make_voter("alice"), block, 700, false, ts(101))
            .unwrap();
        assert_eq!(
            elections.soft_quorum_needing_final(&our_rep),
            vec![(root, block)]
        );

        // We cast a small FINAL vote (100 < 670 → does NOT confirm), but now we
        // HAVE final-voted → no longer flagged (excluded by the is_final check,
        // not by confirmation).
        elections
            .process_vote(&root, &our_rep, block, 100, true, ts(102))
            .unwrap();
        assert!(!elections.get_election(&root).unwrap().is_confirmed());
        assert!(elections.soft_quorum_needing_final(&our_rep).is_empty());
    }

    #[test]
    fn fork_resolution_confirms_accepted_and_identifies_loser() {
        // A final-vote majority selects one fork. This fixture identifies the
        // losing tally entry but does not perform ledger rollback.
        let mut elections = ActiveElections::new(10, 1000);
        let root = make_hash(1);
        let accepted = make_hash(2);
        let fork = make_hash(3);
        let our_rep = make_voter("self");
        elections.start_election(root, ts(100)).unwrap();

        // Minority vote for the fork; our majority (soft) vote for accepted.
        elections
            .process_vote(&root, &make_voter("attacker"), fork, 100, false, ts(101))
            .unwrap();
        elections
            .process_vote(&root, &our_rep, accepted, 800, false, ts(102))
            .unwrap();

        // Accepted leads by 700 ≥ 670 → soft quorum; we haven't finalized yet.
        assert_eq!(
            elections.soft_quorum_needing_final(&our_rep),
            vec![(root, accepted)]
        );

        // The confirmation task's job: finalize. 800 final ≥ 670 → confirm.
        let status = elections
            .process_vote(&root, &our_rep, accepted, 800, true, ts(103))
            .unwrap()
            .expect("accepted block should confirm on final quorum");
        assert_eq!(status.winner, accepted);

        // The fork is the loser (rollback candidate); the accepted block is not.
        let losers = elections.get_fork_losers(&root);
        assert!(
            losers.contains(&fork),
            "fork block must be a rollback loser"
        );
        assert!(
            !losers.contains(&accepted),
            "accepted block must not be rolled back"
        );
    }

    #[test]
    fn process_vote_on_confirmed_election_errors() {
        let mut elections = ActiveElections::new(10, 1000);
        elections.start_election(make_hash(1), ts(100)).unwrap();
        elections
            .process_vote(
                &make_hash(1),
                &make_voter("alice"),
                make_hash(2),
                700,
                true,
                ts(101),
            )
            .unwrap();

        let result = elections.process_vote(
            &make_hash(1),
            &make_voter("bob"),
            make_hash(3),
            100,
            false,
            ts(102),
        );
        assert!(matches!(
            result,
            Err(ConsensusError::ElectionAlreadyConfirmed)
        ));
    }

    #[test]
    fn process_vote_final_vote_already_cast() {
        let mut elections = ActiveElections::new(10, 1000);
        elections.start_election(make_hash(1), ts(100)).unwrap();

        elections
            .process_vote(
                &make_hash(1),
                &make_voter("alice"),
                make_hash(2),
                300,
                true,
                ts(101),
            )
            .unwrap();

        let result = elections.process_vote(
            &make_hash(1),
            &make_voter("alice"),
            make_hash(3),
            300,
            false,
            ts(102),
        );
        assert!(matches!(
            result,
            Err(ConsensusError::FinalVoteAlreadyCast(_))
        ));
    }

    #[test]
    fn cleanup_expired_elections() {
        let mut elections = ActiveElections::new(10, 1000);
        elections.start_election(make_hash(1), ts(100)).unwrap();
        elections.start_election(make_hash(2), ts(110)).unwrap();

        // 30s timeout: election 1 created at 100, now = 131 => expired
        // election 2 created at 110, now = 131 => 21s < 30s => not expired
        let expired = elections.cleanup_expired(30_000, ts(131));

        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0], make_hash(1));
        assert_eq!(elections.election_count(), 1);
        assert!(elections.get_election(&make_hash(1)).is_none());
        assert!(elections.get_election(&make_hash(2)).is_some());
    }

    #[test]
    fn indexed_timeout_matches_scan_across_clock_and_mutation_boundaries() {
        let mut indexed = ActiveElections::new(256, 1000);
        for n in 1..=128 {
            indexed
                .start_election(make_hash(n), ts(90 + u64::from(n % 32)))
                .unwrap();
        }
        let mut reference = indexed.elections.clone();
        for (step, (timeout, now)) in [
            (30_001, 120),
            (31_000, 121),
            (1, 95),
            (u64::MAX, u64::MAX),
            (0, 0),
        ]
        .into_iter()
        .enumerate()
        {
            for (n, created, state) in [
                (126, u64::MAX, ElectionState::Active),
                (127, 0, ElectionState::Confirmed),
                (128, 100, ElectionState::Expired),
            ] {
                if let Some(election) = indexed.get_election_mut(&make_hash(n)) {
                    election.created_at = ts(created);
                    election.state = state;
                    *reference.get_mut(&make_hash(n)).unwrap() = election.clone();
                }
            }
            // Repeat mutable access before cleanup, and revive a retained
            // terminal election after its original deadline was processed.
            if step > 0 {
                if let Some(election) = indexed.get_election_mut(&make_hash(128)) {
                    election.state = ElectionState::Active;
                    *reference.get_mut(&make_hash(128)).unwrap() = election.clone();
                }
            }
            let expected: BTreeSet<_> = reference
                .iter_mut()
                .filter_map(|(root, election)| {
                    election.check_timeout(timeout, ts(now)).then_some(*root)
                })
                .collect();
            for root in &expected {
                reference.remove(root);
            }
            assert_eq!(
                indexed
                    .cleanup_expired(timeout, ts(now))
                    .into_iter()
                    .collect::<BTreeSet<_>>(),
                expected
            );
            assert_eq!(
                indexed.elections.keys().collect::<BTreeSet<_>>(),
                reference.keys().collect()
            );
        }
    }

    #[test]
    fn timeout_index_handles_retirement_recreation_and_authority_reset() {
        let mut elections = ActiveElections::new(10, 1000);
        let root = make_hash(1);
        elections.start_election(root, ts(100)).unwrap();
        elections.get_election_mut(&root).unwrap().created_at = ts(0);
        elections.remove_root(&root);
        elections.start_election(root, ts(200)).unwrap();
        assert!(elections.cleanup_expired(1000, ts(200)).is_empty());
        assert_eq!(elections.cleanup_expired(1000, ts(201)), vec![root]);
        elections.start_election(root, ts(300)).unwrap();
        elections.get_election_mut(&root).unwrap().created_at = ts(0);
        elections
            .install_authority(
                make_hash(2),
                [(make_voter("alice"), crate::rep_weights::WEIGHT_PER_HUMAN)]
                    .into_iter()
                    .collect(),
                6700,
            )
            .unwrap();
        assert!(elections.timeout_roots.is_empty());
        assert!(elections.timeout_dirty.is_empty());
        elections.start_election(root, ts(400)).unwrap();
        assert!(elections.cleanup_expired(1000, ts(400)).is_empty());
        assert_eq!(elections.cleanup_expired(1000, ts(401)), vec![root]);
    }

    #[test]
    fn cleanup_frees_capacity() {
        let mut elections = ActiveElections::new(2, 1000);
        elections.start_election(make_hash(1), ts(100)).unwrap();
        elections.start_election(make_hash(2), ts(101)).unwrap();

        assert!(elections.is_at_capacity());

        elections.cleanup_expired(1, ts(200)); // Expire all
        assert_eq!(elections.election_count(), 0);
        assert!(!elections.is_at_capacity());

        // Should be able to start new elections
        assert!(elections.start_election(make_hash(3), ts(201)).is_ok());
    }

    #[test]
    fn confirmed_elections_list() {
        let mut elections = ActiveElections::new(10, 1000);
        elections.start_election(make_hash(1), ts(100)).unwrap();
        elections.start_election(make_hash(2), ts(100)).unwrap();

        // Confirm election 1
        elections
            .process_vote(
                &make_hash(1),
                &make_voter("alice"),
                make_hash(10),
                700,
                true,
                ts(105),
            )
            .unwrap();

        // Election 2 stays unconfirmed
        elections
            .process_vote(
                &make_hash(2),
                &make_voter("bob"),
                make_hash(20),
                100,
                false,
                ts(105),
            )
            .unwrap();

        let confirmed = elections.confirmed_elections();
        assert_eq!(confirmed.len(), 1);
        assert_eq!(confirmed[0].winner, make_hash(10));
    }

    #[test]
    fn set_online_weight_affects_new_elections() {
        // Standalone mode has no installed authority. The node's authority-backed
        // elections ignore this setter, as tested above.
        let mut elections = ActiveElections::new(10, 1000);
        elections.start_election(make_hash(1), ts(100)).unwrap();

        elections.set_online_weight(2000);
        elections.start_election(make_hash(2), ts(101)).unwrap();

        // Election 1: threshold = 670 (67% of 1000)
        let e1 = elections.get_election(&make_hash(1)).unwrap();
        assert_eq!(e1.confirmation_threshold(), 670);

        // Election 2: threshold = 1340 (67% of 2000)
        let e2 = elections.get_election(&make_hash(2)).unwrap();
        assert_eq!(e2.confirmation_threshold(), 1340);
    }

    #[test]
    fn multiple_votes_build_to_confirmation() {
        let mut elections = ActiveElections::new(10, 1000);
        elections.start_election(make_hash(1), ts(100)).unwrap();

        // Three voters, each insufficient alone
        let r1 = elections
            .process_vote(
                &make_hash(1),
                &make_voter("alice"),
                make_hash(2),
                250,
                true,
                ts(101),
            )
            .unwrap();
        assert!(r1.is_none());

        let r2 = elections
            .process_vote(
                &make_hash(1),
                &make_voter("bob"),
                make_hash(2),
                200,
                true,
                ts(102),
            )
            .unwrap();
        assert!(r2.is_none());

        // This should push it over 670
        let r3 = elections
            .process_vote(
                &make_hash(1),
                &make_voter("carol"),
                make_hash(2),
                250,
                true,
                ts(103),
            )
            .unwrap();
        assert!(r3.is_some());
        let status = r3.unwrap();
        assert_eq!(status.winner, make_hash(2));
        assert_eq!(status.tally, 700);
    }

    #[test]
    fn expire_election_expires_unconfirmed_election() {
        let mut elections = ActiveElections::new(10, 1000);
        elections.start_election(make_hash(1), ts(100)).unwrap(); // confirmed block's election
        elections.start_election(make_hash(2), ts(100)).unwrap(); // fork block's election

        let result = elections.expire_election(make_hash(2));
        assert_eq!(result, Some(make_hash(2)));

        let fork_election = elections.get_election(&make_hash(2)).unwrap();
        assert!(fork_election.is_expired());
    }

    #[test]
    fn expire_election_does_not_expire_confirmed_election() {
        let mut elections = ActiveElections::new(10, 1000);
        elections.start_election(make_hash(1), ts(100)).unwrap();

        // Confirm election 1
        elections
            .process_vote(
                &make_hash(1),
                &make_voter("alice"),
                make_hash(10),
                700,
                true,
                ts(101),
            )
            .unwrap();

        // Trying to resolve a fork against a confirmed election should be a no-op
        let result = elections.expire_election(make_hash(1));
        assert!(result.is_none());

        let election = elections.get_election(&make_hash(1)).unwrap();
        assert!(election.is_confirmed());
    }

    #[test]
    fn expire_election_returns_none_for_missing_election() {
        let mut elections = ActiveElections::new(10, 1000);
        let result = elections.expire_election(make_hash(99));
        assert!(result.is_none());
    }

    #[test]
    fn get_fork_losers_returns_losing_blocks() {
        let mut elections = ActiveElections::new(10, 1000);
        elections.start_election(make_hash(1), ts(100)).unwrap();

        // Vote for losing block first (below threshold, no auto-confirm)
        elections
            .process_vote(
                &make_hash(1),
                &make_voter("bob"),
                make_hash(20),
                100,
                false,
                ts(101),
            )
            .unwrap();

        // Final tally 800 exceeds the 670 confirmation threshold.
        // Confirmation does not require subtracting the runner-up tally.
        let result = elections
            .process_vote(
                &make_hash(1),
                &make_voter("alice"),
                make_hash(10),
                800,
                true,
                ts(102),
            )
            .unwrap();
        assert!(result.is_some());

        let election = elections.get_election(&make_hash(1)).unwrap();
        assert!(election.is_confirmed());

        let losers = elections.get_fork_losers(&make_hash(1));
        assert_eq!(losers.len(), 1);
        assert_eq!(losers[0], make_hash(20));
    }

    #[test]
    fn get_fork_losers_empty_for_unconfirmed() {
        let mut elections = ActiveElections::new(10, 1000);
        elections.start_election(make_hash(1), ts(100)).unwrap();

        // Not enough weight to confirm
        elections
            .process_vote(
                &make_hash(1),
                &make_voter("alice"),
                make_hash(10),
                100,
                false,
                ts(101),
            )
            .unwrap();

        let losers = elections.get_fork_losers(&make_hash(1));
        assert!(losers.is_empty());
    }

    #[test]
    fn get_fork_losers_empty_for_missing_election() {
        let elections = ActiveElections::new(10, 1000);
        let losers = elections.get_fork_losers(&make_hash(99));
        assert!(losers.is_empty());
    }
    #[test]
    fn retiring_one_root_preserves_other_candidate_routes_and_capacity() {
        let mut elections = ActiveElections::new(1024, 1000);
        for i in 1u64..=1000 {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&i.to_be_bytes());
            let root = BlockHash::new(bytes);
            for candidate in 0..16 {
                bytes[8] = candidate;
                elections
                    .register_candidate(root, BlockHash::new(bytes), ts(100))
                    .unwrap();
            }
        }
        let mut removed = [0; 32];
        removed[..8].copy_from_slice(&500u64.to_be_bytes());
        let root = BlockHash::new(removed);
        elections.remove_root(&root);
        elections.remove_root(&root);
        assert_eq!(elections.election_count(), 999);
        assert_eq!(elections.candidate_roots.len(), 999 * 16);
        assert_eq!(elections.candidates_by_root.len(), 999);
        for candidate in 0..16 {
            removed[8] = candidate;
            assert_eq!(elections.candidate_root(&BlockHash::new(removed)), None);
        }
        for (candidate, other_root) in &elections.candidate_roots {
            assert!(elections.get_election(other_root).is_some());
            assert!(elections.candidates_by_root[other_root].contains(candidate));
        }
        elections.register_candidate(root, root, ts(101)).unwrap();
        assert_eq!(elections.candidate_root(&root), Some(root));
        assert_eq!(elections.election_count(), 1000);
    }

    #[test]
    fn candidate_requests_bound_scans_rotate_forks_and_survive_retirement() {
        let hash = |n: u64| {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&n.to_be_bytes());
            BlockHash::new(bytes)
        };
        let mut elections = ActiveElections::new(1024, 1000);
        for n in 0..600 {
            elections.start_election(hash(n), ts(100)).unwrap();
        }
        elections
            .register_candidate(hash(600), hash(600), ts(100))
            .unwrap();
        assert!(elections.next_unconfirmed_candidates(8, 256).is_empty());
        assert_eq!(elections.request_cursor, Some((hash(255), usize::MAX)));
        assert!(elections.next_unconfirmed_candidates(8, 256).is_empty());
        assert_eq!(elections.request_cursor, Some((hash(511), usize::MAX)));
        assert_eq!(
            elections.next_unconfirmed_candidates(8, 256),
            vec![hash(600)]
        );
        let cursor = elections.request_cursor;
        assert!(elections.next_unconfirmed_candidates(0, 256).is_empty());
        assert!(elections.next_unconfirmed_candidates(8, 0).is_empty());
        assert_eq!(elections.request_cursor, cursor);
        for n in 0..=600 {
            elections.remove_root(&hash(n));
        }
        for n in 0..16 {
            elections
                .register_candidate(hash(700), hash(800 + n), ts(101))
                .unwrap();
        }
        assert_eq!(
            elections.next_unconfirmed_candidates(8, 256),
            (800..808).map(hash).collect::<Vec<_>>()
        );
        assert_eq!(
            elections.next_unconfirmed_candidates(8, 256),
            (808..816).map(hash).collect::<Vec<_>>()
        );
        assert_eq!(elections.next_unconfirmed_candidates(32, 256).len(), 16);
        // A root may finish after only a prefix of its candidates was visited.
        elections.next_unconfirmed_candidates(4, 256);
        elections.get_election_mut(&hash(700)).unwrap().state = ElectionState::Confirmed;
        elections
            .register_candidate(hash(900), hash(901), ts(102))
            .unwrap();
        assert_eq!(
            elections.next_unconfirmed_candidates(8, 256),
            vec![hash(901)]
        );
        elections.remove_root(&hash(900));
        assert!(elections.next_unconfirmed_candidates(8, 256).is_empty());
        elections.get_election_mut(&hash(700)).unwrap().state = ElectionState::Expired;
        assert!(elections.next_unconfirmed_candidates(8, 256).is_empty());
        elections
            .install_authority(
                make_hash(9),
                HashMap::from([(make_voter("alice"), 10_000)]),
                6700,
            )
            .unwrap();
        assert!(elections.request_cursor.is_none());
        elections
            .register_candidate(hash(1), hash(2), ts(103))
            .unwrap();
        assert_eq!(elections.next_unconfirmed_candidates(8, 256), vec![hash(2)]);
    }

    #[test]
    fn handoff_pages_survive_vote_consumption_failures_and_reset() {
        let mut elections = ActiveElections::new(512, 1000);
        let roots: Vec<_> = (0u64..300)
            .map(|n| {
                let mut bytes = [0; 32];
                bytes[..8].copy_from_slice(&n.to_be_bytes());
                BlockHash::new(bytes)
            })
            .collect();
        let voter = make_voter("alice");
        for &root in &roots {
            elections.register_candidate(root, root, ts(100)).unwrap();
            elections
                .process_signed_vote(
                    &root,
                    crate::vote_info::VoteInfo::new(voter.clone(), root, 700, true, ts(101), 1),
                    ts(101),
                )
                .unwrap();
        }
        while !elections.confirmation_roots(false, 128).is_empty() {}
        assert!(elections.next_handoff_roots(0).is_empty());
        let mut seen = BTreeSet::new();
        for _ in 0..3 {
            let page = elections.next_handoff_roots(128);
            assert_eq!(page.len(), 128);
            assert!(elections
                .cleanup_confirmed_roots(&page, |_| false)
                .is_empty());
            seen.extend(page);
        }
        assert_eq!(seen, roots.iter().copied().collect());
        elections.remove_root(&roots[0]);
        // Public mutable access can reopen an election or confirm it again.
        elections.get_election_mut(&roots[1]).unwrap().state = ElectionState::Active;
        let page = elections.next_handoff_roots(512);
        assert_eq!(page.len(), 298);
        assert!(!page.contains(&roots[0]) && !page.contains(&roots[1]));
        elections.get_election_mut(&roots[1]).unwrap().state = ElectionState::Confirmed;
        assert!(elections.next_handoff_roots(512).contains(&roots[1]));
        elections
            .install_authority(
                make_hash(9),
                [(voter, crate::rep_weights::WEIGHT_PER_HUMAN)]
                    .into_iter()
                    .collect(),
                6700,
            )
            .unwrap();
        assert!(elections.next_handoff_roots(512).is_empty());
        assert!(elections.handoff_cursor.is_none());
    }

    #[test]
    fn repair_pages_are_bounded_fair_and_preserve_changed_work() {
        let mut elections = ActiveElections::new(1024, 1000);
        let roots: Vec<_> = (0u64..600)
            .map(|i| {
                let mut bytes = [0; 32];
                bytes[..8].copy_from_slice(&i.to_be_bytes());
                BlockHash::new(bytes)
            })
            .collect();
        for &root in &roots {
            elections.register_candidate(root, root, ts(100)).unwrap();
        }
        elections.set_confirmation_wake(Arc::new(|| {}));
        assert!(elections.confirmation_roots(true, 0).is_empty());
        assert_eq!(elections.confirmation_roots(true, 256), roots[..256]);
        // A hot low root must not reset repair progress or discard other work.
        elections.get_election_mut(&roots[0]).unwrap();
        assert_eq!(elections.confirmation_roots(true, 256), roots[256..512]);
        let expected: Vec<_> = std::iter::once(roots[0])
            .chain(roots[512..].iter().copied())
            .collect();
        assert_eq!(elections.confirmation_roots(false, 256), expected);
        let expected: Vec<_> = roots[512..].iter().chain(&roots[..168]).copied().collect();
        assert_eq!(elections.confirmation_roots(true, 256), expected);
        elections.remove_root(&roots[167]); // Cursor deletion must not restart the scan.
        elections.remove_root(&roots[168]);
        assert_eq!(elections.confirmation_roots(true, 1), vec![roots[169]]);
        for &root in &roots {
            elections.remove_root(&root);
        }
        assert!(elections.repair_roots.is_empty());
        assert!(elections.confirmation_roots(true, 256).is_empty());
        elections
            .register_candidate(roots[0], roots[0], ts(101))
            .unwrap();
        assert_eq!(elections.confirmation_roots(true, 256), vec![roots[0]]);
        elections
            .install_authority(
                make_hash(9),
                HashMap::from([(make_voter("alice"), 10_000)]),
                6700,
            )
            .unwrap();
        assert!(elections.repair_roots.is_empty());
        assert!(elections.repair_cursor.is_none());
        assert!(elections.confirmation_roots(true, 256).is_empty());
    }

    #[test]
    fn changed_pages_do_not_starve_dependency_waiters_behind_hot_low_roots() {
        let mut elections = ActiveElections::new(1024, 1000);
        let roots: Vec<_> = (0u64..600)
            .map(|i| {
                let mut bytes = [0; 32];
                bytes[..8].copy_from_slice(&i.to_be_bytes());
                BlockHash::new(bytes)
            })
            .collect();
        for &root in &roots {
            elections.register_candidate(root, root, ts(100)).unwrap();
        }
        elections.set_confirmation_wake(Arc::new(|| {}));
        assert!(elections.confirmation_roots(false, 0).is_empty());
        assert!(elections.changed_cursor.is_none());
        assert_eq!(elections.confirmation_roots(false, 256), roots[..256]);
        // Refill the entire first page: restarting from the lowest key would
        // return it forever, even with a newly cemented dependency at the end.
        for &root in &roots[..256] {
            elections.mark_changed(root);
        }
        assert_eq!(elections.confirmation_roots(false, 256), roots[256..512]);
        let dependency = make_hash(250);
        let generation = elections.dependency_generation();
        elections.wait_for_dependencies(roots[599], roots[599], [dependency], generation);
        elections.dependencies_cemented(&[dependency]);
        for &root in &roots[..512] {
            elections.mark_changed(root);
        }
        let page = elections.confirmation_roots(false, 256);
        assert_eq!(&page[..88], &roots[512..]);
        assert_eq!(&page[88..], &roots[..168]);
        assert!(page.contains(&roots[599]));
        // Deleting the cursor and a later root does not restart or duplicate work.
        elections.remove_root(&roots[167]);
        elections.remove_root(&roots[168]);
        assert_eq!(elections.confirmation_roots(false, 1), vec![roots[169]]);
        // Repair can consume changed roots without disturbing event fairness.
        assert_eq!(elections.confirmation_roots(true, 256).len(), 256);
        assert_eq!(elections.confirmation_roots(false, 1), vec![roots[258]]);
        for &root in &roots {
            elections.remove_root(&root);
        }
        assert!(elections.changed_roots.is_empty());
        assert!(elections.confirmation_roots(false, 256).is_empty());
        elections
            .register_candidate(roots[0], roots[0], ts(101))
            .unwrap();
        elections.mark_changed(roots[0]);
        assert_eq!(elections.confirmation_roots(false, 256), vec![roots[0]]);
        elections
            .install_authority(
                make_hash(9),
                HashMap::from([(make_voter("alice"), 10_000)]),
                6700,
            )
            .unwrap();
        assert!(elections.changed_cursor.is_none());
        assert!(elections.confirmation_roots(false, 256).is_empty());
    }

    #[test]
    fn confirmation_work_coalesces_and_scoped_phases_preserve_repair() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let mut elections = ActiveElections::new(16, 1000);
        let roots = [make_hash(1), make_hash(2), make_hash(3)];
        for root in roots {
            elections.register_candidate(root, root, ts(100)).unwrap();
        }
        let wakes = Arc::new(AtomicUsize::new(0));
        let count = wakes.clone();
        elections.set_confirmation_wake(Arc::new(move || {
            count.fetch_add(1, Ordering::Relaxed);
        }));
        assert_eq!(elections.confirmation_roots(false, 2).len(), 2);
        assert_eq!(elections.confirmation_roots(false, 2), vec![roots[2]]);
        assert!(wakes.load(Ordering::Relaxed) >= 2);
        let voter = make_voter("alice");
        let vote = crate::vote_info::VoteInfo::new(voter.clone(), roots[0], 700, false, ts(101), 1);
        elections
            .process_signed_vote(&roots[0], vote.clone(), ts(101))
            .unwrap();
        let before_duplicate = wakes.load(Ordering::Relaxed);
        elections
            .process_signed_vote(&roots[0], vote, ts(101))
            .unwrap();
        assert_eq!(wakes.load(Ordering::Relaxed), before_duplicate);
        let changed = elections.confirmation_roots(false, 256);
        assert_eq!(changed, vec![roots[0]]);
        assert_eq!(
            elections.soft_quorum_for_roots(&changed, &voter, false),
            vec![(roots[0], roots[0])]
        );
        assert!(elections
            .soft_quorum_for_roots(&[roots[1]], &voter, false)
            .is_empty());
        assert!(elections.confirmed_for_roots(&changed).is_empty());
        elections
            .process_signed_vote(
                &roots[0],
                crate::vote_info::VoteInfo::new(voter.clone(), roots[0], 700, true, ts(102), 2),
                ts(102),
            )
            .unwrap();
        let changed = elections.confirmation_roots(false, 256);
        assert_eq!(elections.confirmed_for_roots(&changed)[0].winner, roots[0]);
        assert!(elections
            .soft_quorum_for_roots(&changed, &voter, false)
            .is_empty());
        assert!(elections
            .cleanup_confirmed_roots(&changed, |_| false)
            .is_empty());
        assert!(elections.confirmation_roots(false, 256).is_empty());
        assert!(elections.confirmation_roots(true, 256).contains(&roots[0]));
        assert_eq!(
            elections.cleanup_confirmed_roots(&changed, |_| true),
            vec![roots[0]]
        );
        assert!(elections.get_election(&roots[1]).is_some());
        elections.get_election_mut(&roots[1]).unwrap();
        assert_eq!(elections.confirmation_roots(false, 256), vec![roots[1]]);
        elections.get_election_mut(&roots[2]).unwrap();
        elections
            .install_authority(make_hash(9), HashMap::from([(voter, 10_000)]), 6700)
            .unwrap();
        assert!(elections.confirmation_roots(false, 256).is_empty());
        assert!(elections.confirmation_roots(true, 256).is_empty());
    }

    #[test]
    fn cementation_wakes_only_dependency_waiters_and_rechecks_remaining_sources() {
        let mut elections = ActiveElections::new(128, 1000);
        for i in 1..=100 {
            let root = make_hash(i);
            elections.register_candidate(root, root, ts(100)).unwrap();
        }
        elections.confirmation_roots(false, 256);
        let roots = [make_hash(1), make_hash(2)];
        let source = make_hash(201);
        let predecessor = make_hash(202);
        let generation = elections.dependency_generation();
        elections.wait_for_dependencies(
            roots[0],
            roots[0],
            [source, predecessor, source],
            generation,
        );
        elections.wait_for_dependencies(roots[1], roots[1], [predecessor], generation);
        elections.dependencies_cemented(&[make_hash(203)]);
        assert!(elections.confirmation_roots(false, 256).is_empty());
        elections.dependencies_cemented(&[source]);
        assert_eq!(elections.confirmation_roots(false, 256), vec![roots[0]]);
        // A wake does not assert finality of the other dependency. The caller's
        // durable check can register the remaining predecessor again.
        elections.wait_for_dependencies(
            roots[0],
            roots[0],
            [predecessor],
            elections.dependency_generation(),
        );
        elections.dependencies_cemented(&[predecessor, predecessor]);
        let awakened = elections.confirmation_roots(false, 256);
        assert_eq!(awakened.len(), roots.len());
        assert_eq!(
            awakened.into_iter().collect::<BTreeSet<_>>(),
            roots.iter().copied().collect()
        );
        assert!(elections.waiting_dependencies.is_empty());
        assert!(elections.dependent_roots.is_empty());
        assert!(elections.confirmed_for_roots(&roots).is_empty());
    }

    #[test]
    fn dependency_wait_batches_bound_old_and_new_edges_and_preserve_races() {
        let mut elections = ActiveElections::new(80, 1000);
        let waits: Vec<_> = (1..=70)
            .map(|n| {
                let root = make_hash(n);
                elections.register_candidate(root, root, ts(100)).unwrap();
                (root, root, vec![make_hash(if n == 1 { 200 } else { 201 })])
            })
            .collect();
        let generation = elections.dependency_generation();
        elections.dependencies_cemented(&[make_hash(200)]);
        elections.remove_root(&make_hash(2));
        let mut applied = 0;
        for expected in [32, 32, 6] {
            let count = elections.wait_for_dependencies_batch(&waits[applied..], generation);
            assert_eq!(count, expected);
            applied += count;
        }
        assert_eq!(applied, waits.len());
        assert!(
            !elections.waiting_dependencies.contains_key(&make_hash(1)),
            "raced cementation must retry"
        );
        assert!(
            !elections.waiting_dependencies.contains_key(&make_hash(2)),
            "removed root must not return"
        );
        assert_eq!(elections.waiting_dependencies.len(), 68);
        assert!(elections
            .confirmation_roots(false, 256)
            .contains(&make_hash(1)));
        let generation = elections.dependency_generation();
        let edges: Vec<_> = (0..1500u32)
            .map(|n| {
                let mut bytes = [0; 32];
                bytes[..4].copy_from_slice(&n.to_le_bytes());
                BlockHash::new(bytes)
            })
            .collect();
        elections.wait_for_dependencies(make_hash(3), make_hash(3), edges, generation);
        assert_eq!(
            elections.wait_for_dependencies_batch(&waits[2..], generation),
            1,
            "removing a large previous wait must occupy its own lock batch"
        );
        let large = vec![
            (make_hash(4), make_hash(4), vec![make_hash(202); 1500]),
            waits[4].clone(),
        ];
        assert_eq!(
            elections.wait_for_dependencies_batch(&large, generation),
            1,
            "oversized input must progress alone"
        );
        elections.dependencies_cemented(&[make_hash(201), make_hash(202)]);
        assert!(elections.waiting_dependencies.is_empty());
        assert!(elections.dependent_roots.is_empty());
    }

    #[test]
    fn dependency_wait_suppresses_vote_churn_but_preserves_forks_repair_and_wakes() {
        let mut elections = ActiveElections::new(8, 1000);
        let root = make_hash(1);
        let first = make_hash(2);
        let fork = make_hash(3);
        let dependency = make_hash(4);
        let voter = make_voter("alice");
        let ours = make_voter("self");
        elections.register_candidate(root, first, ts(100)).unwrap();
        elections.register_candidate(root, fork, ts(100)).unwrap();
        let vote = |hash, sequence| {
            crate::vote_info::VoteInfo::new(voter.clone(), hash, 700, false, ts(101), sequence)
        };
        elections
            .process_signed_vote(&root, vote(first, 1), ts(101))
            .unwrap();
        elections.wait_for_dependencies(
            root,
            first,
            [dependency],
            elections.dependency_generation(),
        );
        for sequence in 2..102 {
            elections
                .process_signed_vote(&root, vote(first, sequence), ts(101))
                .unwrap();
            let changed = elections.confirmation_roots(false, 256);
            assert_eq!(changed, vec![root]);
            assert!(elections
                .soft_quorum_for_roots(&changed, &ours, false)
                .is_empty());
            // Waiting for final-vote dependencies must not hide confirmation itself.
            assert!(elections.confirmed_for_roots(&changed).is_empty());
        }
        let repaired = elections.confirmation_roots(true, 256);
        assert_eq!(
            elections.soft_quorum_for_roots(&repaired, &ours, true),
            vec![(root, first)]
        );
        elections
            .process_signed_vote(&root, vote(fork, 102), ts(101))
            .unwrap();
        let changed = elections.confirmation_roots(false, 256);
        assert_eq!(
            elections.soft_quorum_for_roots(&changed, &ours, false),
            vec![(root, fork)]
        );
        // An old worker result may register after the leading candidate changes.
        elections.wait_for_dependencies(
            root,
            first,
            [dependency],
            elections.dependency_generation(),
        );
        assert_eq!(
            elections.soft_quorum_for_roots(&changed, &ours, false),
            vec![(root, fork)]
        );
        elections.wait_for_dependencies(
            root,
            fork,
            [dependency],
            elections.dependency_generation(),
        );
        assert!(elections
            .soft_quorum_for_roots(&changed, &ours, false)
            .is_empty());
        elections.dependencies_cemented(&[dependency]);
        let changed = elections.confirmation_roots(false, 256);
        assert_eq!(
            elections.soft_quorum_for_roots(&changed, &ours, false),
            vec![(root, fork)]
        );
        let before = elections.dependency_generation();
        elections.dependencies_cemented(&[dependency]);
        elections.wait_for_dependencies(root, fork, [dependency], before);
        let changed = elections.confirmation_roots(false, 256);
        assert_eq!(
            elections.soft_quorum_for_roots(&changed, &ours, false),
            vec![(root, fork)]
        );
        elections.wait_for_dependencies(
            root,
            fork,
            [dependency],
            elections.dependency_generation(),
        );
        elections
            .process_signed_vote(
                &root,
                crate::vote_info::VoteInfo::new(voter, fork, 700, true, ts(102), 103),
                ts(102),
            )
            .unwrap();
        let changed = elections.confirmation_roots(false, 256);
        assert_eq!(elections.confirmed_for_roots(&changed).len(), 1);
        assert_eq!(
            elections.cleanup_confirmed_roots(&changed, |_| true),
            vec![root]
        );
        assert!(elections.waiting_dependencies.is_empty());
        assert!(elections.dependent_roots.is_empty());
    }

    #[test]
    fn dependency_registration_race_retirement_and_epoch_change_do_not_lose_or_retain_work() {
        let mut elections = ActiveElections::new(8, 1000);
        let root = make_hash(1);
        let dependency = make_hash(2);
        elections.register_candidate(root, root, ts(100)).unwrap();
        elections.confirmation_roots(false, 256);
        let checked_generation = elections.dependency_generation();
        elections.dependencies_cemented(&[make_hash(7)]);
        elections.wait_for_dependencies(root, root, [dependency], checked_generation);
        assert!(elections.confirmation_roots(false, 256).is_empty());
        assert!(elections.dependent_roots[&dependency].contains(&root));
        // An actual dependency still wakes a previously registered waiter.
        elections.dependencies_cemented(&[dependency]);
        assert_eq!(elections.confirmation_roots(false, 256), vec![root]);
        let checked_generation = elections.dependency_generation();
        elections.dependencies_cemented(&[dependency]);
        elections.wait_for_dependencies(root, root, [dependency], checked_generation);
        assert_eq!(elections.confirmation_roots(false, 256), vec![root]);
        assert!(elections.dependent_roots.is_empty());
        elections.wait_for_dependencies(
            root,
            root,
            [dependency],
            elections.dependency_generation(),
        );
        elections.remove_root(&root);
        assert!(elections.dependent_roots.is_empty());
        elections.wait_for_dependencies(
            root,
            root,
            [dependency],
            elections.dependency_generation(),
        );
        elections.dependencies_cemented(&[dependency]);
        assert!(elections.confirmation_roots(false, 256).is_empty());
        elections.register_candidate(root, root, ts(101)).unwrap();
        elections.confirmation_roots(false, 256);
        elections.wait_for_dependencies(
            root,
            root,
            [dependency],
            elections.dependency_generation(),
        );
        let generation = elections.dependency_generation();
        elections
            .install_authority(
                make_hash(9),
                HashMap::from([(make_voter("alice"), 10_000)]),
                6700,
            )
            .unwrap();
        assert_ne!(generation, elections.dependency_generation());
        assert!(elections.waiting_dependencies.is_empty());
        assert!(elections.dependent_roots.is_empty());
        elections.dependencies_cemented(&[dependency]);
        assert!(elections.confirmation_roots(false, 256).is_empty());
    }
}
