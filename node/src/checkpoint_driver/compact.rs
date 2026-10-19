use super::*;

impl CheckpointDriver {
    /// Return a conservative replacement cut only for an expired, unprepared
    /// selection in a later round. This never changes the durable round state.
    pub fn compact_refresh_cut(
        &self,
        clock: Option<SigningClock>,
    ) -> Result<Option<u64>, StoreError> {
        let Some(clock) = clock else { return Ok(None) };
        let Some(candidates) = &self.compact else {
            return Ok(None);
        };
        if self.round == 0
            || self.state()?.is_some_and(|state| {
                state.step() != Step::Propose
                    || state.proposal().is_some()
                    || state.valid_proof().is_some()
                    || state.decision().is_some()
            })
        {
            return Ok(None);
        }
        let previous = candidates
            .iter()
            .find(|candidate| candidate.input().manifest() == self.initial)
            .ok_or_else(|| invalid("compact initial candidate is missing"))?;
        let CompactReplayCandidate::Selection(previous) = previous else {
            return Ok(None);
        };
        let Some(earliest) = clock.now.checked_sub(clock.error_bound) else {
            return Ok(None);
        };
        if earliest < previous.replay().request().beacon_time().map_err(invalid)? {
            return Ok(None);
        }
        Ok(earliest.checked_sub(1))
    }

    /// Install a newly discovered candidate in the proposal cache, not the
    /// journal. The signing transition still gives a saved prepare proof priority.
    pub fn replace_compact_selection(
        &mut self,
        candidate: CompactSelectionCoverage,
        clock: Option<SigningClock>,
    ) -> Result<bool, StoreError> {
        let observed = clock.map(ObservedSigningClock::new);
        let Some(cutoff) = self.compact_refresh_cut(clock)? else {
            return Ok(false);
        };
        let checkpoint = candidate.replay().checkpoint();
        if checkpoint.cutoff <= self.initial.checkpoint().cutoff || checkpoint.cutoff > cutoff {
            return Err(invalid("compact replacement is not a newer safe cut"));
        }
        candidate
            .replay()
            .validate_parent_commitments(&self.anchor)?;
        let manifest = ReplayManifest::Selection(candidate.replay().manifest());
        let batch = self.store.tx_begin_write()?;
        candidate.check_signing_parent(&batch, &self.anchor)?;
        let clock = observed
            .and_then(ObservedSigningClock::current)
            .ok_or_else(|| invalid("compact replacement has no current clock bound"))?;
        candidate
            .replay()
            .request()
            .validate_before_signing(checkpoint, clock.now, clock.error_bound)
            .map_err(invalid)?;
        drop(batch);
        self.cache_compact_selection(candidate)?;
        self.initial = manifest;
        self.prune_replays()?;
        Ok(true)
    }

    pub(crate) fn is_compact(&self) -> bool {
        self.compact.is_some()
    }

    pub(crate) fn needs_compact_replay(&self, manifest: &ReplayManifest) -> bool {
        self.compact.as_ref().is_some_and(|candidates| {
            !candidates
                .iter()
                .any(|candidate| candidate.input().manifest() == *manifest)
        })
    }

    /// Run the same single-height agreement rules on compact replay coverage.
    /// Retained candidates must come from journal recovery; missing choices
    /// reject construction. This does not enable compact runtime rotation.
    pub fn new_compact_selection(
        store: Arc<LmdbStore>,
        params: ProtocolParams,
        key: Arc<PrivateKey>,
        anchor: CheckpointAnchor,
        initial: CompactSelectionCoverage,
        retained: Vec<CompactSelectionCoverage>,
    ) -> Result<Self, StoreError> {
        Self::new_compact(
            store,
            params,
            key,
            anchor,
            CompactReplayCandidate::Selection(Box::new(initial)),
            retained
                .into_iter()
                .map(|value| CompactReplayCandidate::Selection(Box::new(value)))
                .collect(),
        )
    }

    pub fn new_compact(
        store: Arc<LmdbStore>,
        params: ProtocolParams,
        key: Arc<PrivateKey>,
        anchor: CheckpointAnchor,
        initial: CompactReplayCandidate,
        mut retained: Vec<CompactReplayCandidate>,
    ) -> Result<Self, StoreError> {
        if !cfg!(test) && !burst_verification::checkpoint_selection_enabled(anchor.network()) {
            return Err(invalid(
                "compact checkpoint driver is behind the release gate",
            ));
        }
        let representative = derive_address(&public_from_private(&key));
        let manifest = initial.input().manifest();
        if !retained
            .iter()
            .any(|candidate| candidate.input().manifest() == manifest)
        {
            retained.push(initial);
        }
        if retained.len() > 14 {
            return Err(invalid("checkpoint replay cache is full"));
        }
        for saved in
            checkpoint_agreement::read_retained_manifests(&store, &anchor, &representative)?
        {
            if !retained
                .iter()
                .any(|candidate| candidate.input().manifest() == saved)
            {
                return Err(invalid(
                    "compact driver is missing a retained replay candidate",
                ));
            }
        }
        let state = checkpoint_agreement::read_state(&store, &anchor, &representative)?;
        let decided = state
            .as_ref()
            .is_some_and(|state| state.decision().is_some());
        let batch = store.tx_begin_write()?;
        for candidate in &retained {
            if decided {
                // A finished height may only relay its verified decision after
                // the chain advances. Resume cannot produce a fresh ballot.
                candidate.input().validate_parent(&anchor)?;
            } else {
                candidate.input().check_signing_parent(&batch, &anchor)?;
            }
        }
        drop(batch);
        let round = state.map_or(0, |state| state.round());
        Ok(Self {
            store,
            params,
            key,
            representative,
            initial: manifest,
            replays: Vec::new(),
            compact: Some(retained),
            round,
            evidence: RoundEvidence::new(anchor.clone(), round),
            future: FutureEvidence::new(anchor.clone()),
            anchor,
            proposals: Vec::new(),
        })
    }

    /// Cache externally authenticated replay before processing a peer proposal.
    /// This cannot replace the initial proposal or clear a durable lock.
    pub fn cache_compact_selection(
        &mut self,
        candidate: CompactSelectionCoverage,
    ) -> Result<(), StoreError> {
        self.cache_compact_candidate(CompactReplayCandidate::Selection(Box::new(candidate)))
    }

    pub fn cache_compact_candidate(
        &mut self,
        candidate: CompactReplayCandidate,
    ) -> Result<(), StoreError> {
        let candidates = self
            .compact
            .as_mut()
            .ok_or_else(|| invalid("compact coverage supplied to a full-history driver"))?;
        let batch = self.store.tx_begin_write()?;
        candidate
            .input()
            .check_signing_parent(&batch, &self.anchor)?;
        drop(batch);
        if candidates
            .iter()
            .any(|saved| saved.input().manifest() == candidate.input().manifest())
        {
            return Ok(());
        }
        if candidates.len() >= 14 {
            return Err(invalid("checkpoint replay cache is full"));
        }
        candidates.push(candidate);
        Ok(())
    }
}
