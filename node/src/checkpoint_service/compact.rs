use super::*;
use crate::{
    checkpoint_agreement, checkpoint_replay::CheckpointManifest,
    confirmed_replay::snapshot::installed::RecoveredSnapshot,
};
use burst_crypto::{derive_address, public_from_private};

pub(super) struct Context {
    store: Arc<burst_store_lmdb::LmdbStore>,
    base: Arc<RecoveredSnapshot>,
}

type PreparedIncoming = Result<(Vec<Send>, Option<Received>), StoreError>;

pub enum CompactStart {
    Ready(Box<Session>),
    Idle(Box<RecoveredSnapshot>),
}

enum StartMode {
    Explicit(Option<Box<checkpoint_agreement::ReplayManifest>>),
    Discover(Option<SigningClock>),
}

impl Session {
    pub(super) async fn refresh_compact_proposal(
        self,
        registry: Arc<RwLock<ConnectionRegistry>>,
        clock: Clock,
    ) -> Result<Self, StoreError> {
        let observed_clock = clock.clone();
        let (session, cut) = tokio::task::spawn_blocking(move || {
            let cut = if self.driver.is_proposer() && Instant::now() >= self.proposal_retry {
                self.driver.compact_refresh_cut(observed_clock())
            } else {
                Ok(None)
            };
            (self, cut)
        })
        .await
        .map_err(|error| StoreError::Corruption(error.to_string()))?;
        let cutoff = match cut {
            Ok(Some(cutoff)) => cutoff,
            Ok(None) => return Ok(session),
            Err(error) => {
                tracing::debug!(%error, "compact proposal refresh deferred");
                return Ok(session);
            }
        };
        let context = session
            .compact_context
            .as_ref()
            .expect("compact session context");
        let candidate = context
            .base
            .clone()
            .discover_cemented_selection_from_peers(context.store.clone(), registry, cutoff)
            .await;
        let candidate = match candidate {
            Ok(Some(candidate)) => candidate,
            Ok(None) => return Ok(session),
            Err(error) => {
                tracing::debug!(%error, "compact proposal refresh replay deferred");
                return Ok(session);
            }
        };
        tokio::task::spawn_blocking(move || {
            let mut session = session;
            // Retrieval can consume the signing window. Recheck time and saved
            // prepare evidence before replacing only the unsigned candidate.
            match session.driver.replace_compact_selection(candidate, clock()) {
                Ok(true) => tracing::debug!(cutoff, "compact proposal refreshed"),
                Ok(false) => (),
                Err(error) => tracing::debug!(%error, "compact proposal refresh deferred"),
            }
            session
        })
        .await
        .map_err(|error| StoreError::Corruption(error.to_string()))
    }

    /// Construct a single-height compact worker only after retained journal
    /// inputs and the initial candidate have authenticated replay coverage.
    /// Callers remain responsible for height rotation and candidate discovery.
    pub async fn from_compact(
        settings: Settings,
        registry: Arc<RwLock<ConnectionRegistry>>,
        recovered: RecoveredSnapshot,
        initial: Option<CheckpointManifest>,
        step_timeout: Duration,
    ) -> Result<Self, StoreError> {
        Self::from_compact_candidate(
            settings,
            registry,
            recovered,
            initial.map(checkpoint_agreement::ReplayManifest::Selection),
            step_timeout,
        )
        .await
    }

    pub async fn from_compact_candidate(
        settings: Settings,
        registry: Arc<RwLock<ConnectionRegistry>>,
        recovered: RecoveredSnapshot,
        initial: Option<checkpoint_agreement::ReplayManifest>,
        step_timeout: Duration,
    ) -> Result<Self, StoreError> {
        Self::from_compact_candidate_shared(
            settings,
            registry,
            Arc::new(recovered),
            initial,
            step_timeout,
        )
        .await
    }

    pub(super) async fn from_compact_candidate_shared(
        settings: Settings,
        registry: Arc<RwLock<ConnectionRegistry>>,
        recovered: Arc<RecoveredSnapshot>,
        initial: Option<checkpoint_agreement::ReplayManifest>,
        step_timeout: Duration,
    ) -> Result<Self, StoreError> {
        match Self::start_compact(
            settings,
            registry,
            recovered,
            StartMode::Explicit(initial.map(Box::new)),
            step_timeout,
        )
        .await?
        {
            Some(session) => Ok(*session),
            None => Err(StoreError::Corruption(
                "explicit compact startup has no candidate".into(),
            )),
        }
    }

    /// Saved choices take precedence over new discovery, including without a
    /// trusted clock. An idle result retains the authenticated base for retry.
    pub async fn resume_or_discover_compact(
        settings: Settings,
        registry: Arc<RwLock<ConnectionRegistry>>,
        recovered: RecoveredSnapshot,
        clock: Option<SigningClock>,
        step_timeout: Duration,
    ) -> Result<CompactStart, StoreError> {
        let recovered = Arc::new(recovered);
        match Self::resume_or_discover_compact_shared(
            settings,
            registry,
            recovered.clone(),
            clock,
            step_timeout,
        )
        .await?
        {
            Some(session) => Ok(CompactStart::Ready(session)),
            None => Arc::try_unwrap(recovered)
                .map(|base| CompactStart::Idle(Box::new(base)))
                .map_err(|_| {
                    StoreError::Corruption("idle compact startup retained its base".into())
                }),
        }
    }

    /// The caller can retain the authenticated base across an idle result or a
    /// temporary proof failure. Every attempt rechecks authority and the journal.
    pub async fn resume_or_discover_compact_shared(
        settings: Settings,
        registry: Arc<RwLock<ConnectionRegistry>>,
        recovered: Arc<RecoveredSnapshot>,
        clock: Option<SigningClock>,
        step_timeout: Duration,
    ) -> Result<Option<Box<Session>>, StoreError> {
        Self::start_compact(
            settings,
            registry,
            recovered,
            StartMode::Discover(clock),
            step_timeout,
        )
        .await
    }

    async fn start_compact(
        settings: Settings,
        registry: Arc<RwLock<ConnectionRegistry>>,
        recovered: Arc<RecoveredSnapshot>,
        mode: StartMode,
        step_timeout: Duration,
    ) -> Result<Option<Box<Session>>, StoreError> {
        let invalid = |message: &str| StoreError::Corruption(message.to_owned());
        if !cfg!(test) && !burst_verification::checkpoint_selection_enabled(settings.network) {
            return Err(invalid(
                "compact checkpoint service is behind the release gate",
            ));
        }
        let key = settings
            .key
            .ok_or_else(|| invalid("observer cannot start a checkpoint signer"))?;
        let anchor = recovered.continuation.anchor().clone();
        if anchor.network() != settings.network {
            return Err(invalid(
                "compact service network differs from recovered authority",
            ));
        }
        let representative = derive_address(&public_from_private(&key));
        let mut retained = checkpoint_agreement::recover_compact_candidates_shared(
            settings.store.clone(),
            registry.clone(),
            recovered.clone(),
            representative,
        )
        .await?;
        let initial = match mode {
            StartMode::Explicit(Some(manifest)) => match retained
                .iter()
                .position(|candidate| candidate.input().manifest() == *manifest)
            {
                Some(index) => retained.remove(index),
                None => {
                    recovered
                        .clone()
                        .candidate_coverage_shared(settings.store.clone(), registry, *manifest)
                        .await?
                }
            },
            _ if !retained.is_empty() => retained.remove(0),
            StartMode::Explicit(None) => {
                return Err(invalid("compact service has no replayed candidate"))
            }
            StartMode::Discover(None) => return Ok(None),
            StartMode::Discover(Some(clock)) => {
                let candidate = recovered
                    .clone()
                    .discover_cemented_candidate_from_peers(settings.store.clone(), registry, clock)
                    .await?;
                let Some(candidate) = candidate else {
                    return Ok(None);
                };
                candidate
            }
        };
        tokio::task::spawn_blocking(move || {
            let context = Context {
                store: settings.store.clone(),
                base: recovered,
            };
            let driver = CheckpointDriver::new_compact(
                settings.store,
                settings.initial_params,
                key,
                anchor,
                initial,
                retained,
            )?;
            let mut session = Self::new(driver, Instant::now(), step_timeout)?;
            session.compact_context = Some(context);
            Ok(Some(Box::new(session)))
        })
        .await
        .map_err(|error| StoreError::Corruption(error.to_string()))?
    }

    pub(super) async fn prepare_compact_incoming(
        self,
        incoming: Incoming,
        registry: Arc<RwLock<ConnectionRegistry>>,
    ) -> Result<(Self, PreparedIncoming), StoreError> {
        let (mut session, prepared) = tokio::task::spawn_blocking(move || {
            let mut session = self;
            let prepared = session
                .transport
                .handle(&incoming.peer, incoming.packet, Instant::now())
                .map(|(reply, received)| (reply.into_iter().collect::<Vec<_>>(), received));
            (session, prepared)
        })
        .await
        .map_err(|error| StoreError::Corruption(error.to_string()))?;
        let (replies, received) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return Ok((session, Err(error))),
        };
        let Some(received) = received else {
            return Ok((session, Ok((replies, None))));
        };
        if session.state.decision().is_some() {
            return Ok((session, Ok((replies, Some(received)))));
        }
        if let Err(error) = session.driver.authenticate(&received.message) {
            return Ok((session, Err(error)));
        }
        let manifest = match &received.message {
            Message::Proposal { manifest, .. }
            | Message::Prepared { manifest, .. }
            | Message::Decision { manifest, .. }
                if session.driver.needs_compact_replay(manifest) =>
            {
                Some(manifest)
            }
            _ => None,
        };
        if let Some(manifest) = manifest {
            let context = session
                .compact_context
                .as_ref()
                .expect("compact session context");
            let coverage = context
                .base
                .clone()
                .candidate_coverage_shared(context.store.clone(), registry, manifest.clone())
                .await;
            let coverage = match coverage {
                Ok(coverage) => coverage,
                Err(error) => return Ok((session, Err(error))),
            };
            return tokio::task::spawn_blocking(move || {
                let result = session
                    .driver
                    .cache_compact_candidate(coverage)
                    .map(|()| (replies, Some(received)));
                (session, result)
            })
            .await
            .map_err(|error| StoreError::Corruption(error.to_string()));
        }
        Ok((session, Ok((replies, Some(received)))))
    }
}
