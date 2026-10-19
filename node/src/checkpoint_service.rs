//! A single-height agreement worker. Socket readers only enqueue bounded packets;
//! replay, durable signing and phase scheduling run on one blocking worker.

use burst_consensus::checkpoint_round::{RoundState, Step};
use burst_store::StoreError;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, RwLock};

use crate::{
    checkpoint_driver::{CheckpointDriver, Message},
    checkpoint_replay::SigningClock,
    checkpoint_transport::{Context, Incoming, Received, Send, Transport},
    connection_registry::ConnectionRegistry,
    shutdown::ShutdownController,
    wire_message::WireMessage,
};

pub const INBOX_CAPACITY: usize = 32;
pub type Clock = Arc<dyn Fn() -> Option<SigningClock> + std::marker::Send + Sync>;

mod candidate;
mod compact;
pub(crate) use candidate::discover_candidate;
pub(crate) mod compact_authority;
pub use compact::CompactStart;
pub use compact_authority::recover_compact_authority;
pub(crate) mod compact_runtime;
pub use compact_runtime::run_compact_node;
pub(crate) mod compact_observer;
pub use compact_observer::{run_compact_height, run_compact_observer};
mod input;
mod runtime;
pub(crate) use runtime::run_node_with_recovery;
#[cfg(test)]
pub(crate) use runtime::Runtime;
pub use runtime::{run_node, Settings};

pub struct Session {
    driver: CheckpointDriver,
    transport: Transport,
    // Scheduling cache only. The durable signing boundary still validates the
    // stored state before every transition; packets do not trigger disk replay.
    state: RoundState,
    phase: Option<(u64, Step)>,
    phase_started: Instant,
    step_timeout: Duration,
    proposal_retry: Instant,
    timeout_retry: Instant,
    resume_at: Instant,
    compact_decision_persisted: bool,
    compact_context: Option<compact::Context>,
}

impl Session {
    pub fn new(
        mut driver: CheckpointDriver,
        now: Instant,
        step_timeout: Duration,
    ) -> Result<Self, StoreError> {
        if step_timeout.is_zero() || step_timeout > Duration::from_secs(60) {
            return Err(StoreError::Corruption(
                "invalid checkpoint step timeout".into(),
            ));
        }
        let mut transport = Transport::new(Context::from_anchor(driver.anchor())?, now);
        for message in driver.resume()? {
            transport.publish(&message)?;
        }
        let state = driver
            .state()?
            .ok_or_else(|| StoreError::Corruption("missing checkpoint round state".into()))?;
        let phase = Some((state.round(), state.step()));
        Ok(Self {
            driver,
            transport,
            state,
            phase,
            phase_started: now,
            step_timeout,
            proposal_retry: now,
            timeout_retry: now,
            resume_at: now + Duration::from_secs(5),
            compact_decision_persisted: false,
            compact_context: None,
        })
    }

    fn publish(&mut self, messages: Vec<Message>) -> Result<(), StoreError> {
        for message in messages {
            self.transport.publish(&message)?;
        }
        Ok(())
    }

    fn receive(
        &mut self,
        incoming: Incoming,
        now: Instant,
        clock: &Clock,
    ) -> Result<Vec<Send>, StoreError> {
        let (reply, received) = self
            .transport
            .handle(&incoming.peer, incoming.packet, now)?;
        if let Some(received) = received {
            self.accept(received, clock)?;
        }
        Ok(reply.into_iter().collect())
    }

    fn accept(&mut self, received: Received, clock: &Clock) -> Result<(), StoreError> {
        // An already decided actor only serves its durable decision. The
        // driver's ignored-message fast path is not a relay authorization.
        if self.state.decision().is_some() {
            self.transport.acknowledge(received.digest);
            return Ok(());
        }
        self.driver.authenticate(&received.message)?;
        let outgoing = self.driver.receive(received.message.clone(), clock())?;
        self.refresh(Instant::now())?;
        self.transport.publish(&received.message)?;
        self.publish(outgoing)?;
        self.transport.acknowledge(received.digest);
        Ok(())
    }

    fn refresh(&mut self, now: Instant) -> Result<(), StoreError> {
        self.state = self
            .driver
            .state()?
            .ok_or_else(|| StoreError::Corruption("missing checkpoint round state".into()))?;
        let phase = (self.state.round(), self.state.step());
        if self.phase != Some(phase) {
            self.phase = Some(phase);
            self.phase_started = now;
            self.timeout_retry = now;
        }
        Ok(())
    }

    fn poll(
        &mut self,
        peers: &[String],
        now: Instant,
        clock: &Clock,
    ) -> Result<Vec<Send>, StoreError> {
        let decided = self.state.decision().is_some();
        if decided && self.driver.is_compact() && !self.compact_decision_persisted {
            self.compact_decision_persisted = self.driver.persist_decision()?;
        }
        if !decided && self.driver.is_proposer() && now >= self.proposal_retry {
            if self.state.step() == Step::Propose && self.state.proposal().is_none() {
                let proposed = self
                    .driver
                    .refresh_candidate(clock())
                    .and_then(|()| self.driver.propose(clock()));
                match proposed {
                    Ok(messages) => {
                        self.refresh(Instant::now())?;
                        self.publish(messages)?;
                    }
                    Err(error) => tracing::debug!(%error, "checkpoint proposal deferred"),
                }
            }
            self.proposal_retry = now + Duration::from_secs(1);
        }
        if self.state.decision().is_none() {
            let (round, step) = self.phase.expect("observed phase");
            let timeout = self
                .step_timeout
                .saturating_mul(round.saturating_add(1).min(12) as u32)
                .min(Duration::from_secs(60));
            if now.saturating_duration_since(self.phase_started) >= timeout
                && now >= self.timeout_retry
            {
                // The driver still requires the phase's human participation
                // certificate. Elapsed time cannot manufacture a quorum.
                match self.driver.timeout(step) {
                    Ok(messages) => self.publish(messages)?,
                    Err(error) => {
                        tracing::debug!(height = self.driver.anchor().height() + 1,
                            round, ?step, %error, "checkpoint timeout awaits participation")
                    }
                }
                self.timeout_retry = now + Duration::from_secs(1);
                self.refresh(Instant::now())?;
            }
        }
        if now >= self.resume_at {
            let messages = self.driver.resume()?;
            self.refresh(Instant::now())?;
            self.publish(messages)?;
            self.resume_at = now + Duration::from_secs(5);
        }
        Ok(self.transport.poll(peers, now))
    }
}

/// The caller supplies a replay-derived candidate and a trusted clock provider.
/// Returning `None` from the provider prohibits fresh value signatures, while
/// allowing previously prepared choices to finish. This worker installs no panel.
pub async fn run(
    mut session: Session,
    registry: Arc<RwLock<ConnectionRegistry>>,
    mut inbox: mpsc::Receiver<Incoming>,
    clock: Clock,
    shutdown: Arc<ShutdownController>,
) -> Result<(), StoreError> {
    let mut stop = shutdown.subscribe();
    let mut timer = tokio::time::interval(Duration::from_millis(250));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let mut incoming = match input::next(&mut stop, &mut inbox, &mut timer).await {
            Ok(incoming) => incoming,
            Err(()) => return Ok(()),
        };
        if session.compact_context.is_some() && incoming.is_none() {
            session = tokio::select! {
                biased;
                _ = stop.recv() => return Ok(()),
                result = session.refresh_compact_proposal(registry.clone(), clock.clone()) => result?,
            };
        }
        let prepared = if session.compact_context.is_some() && incoming.is_some() {
            let task = session.prepare_compact_incoming(incoming.take().unwrap(), registry.clone());
            let (returned, prepared) = tokio::select! {
                biased;
                _ = stop.recv() => return Ok(()),
                result = task => result?,
            };
            session = returned;
            Some(prepared)
        } else {
            None
        };
        let peers = registry
            .read()
            .await
            .peer_ids()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let observed_clock = Arc::clone(&clock);
        let (returned, result) = tokio::task::spawn_blocking(move || {
            let now = Instant::now();
            let result = (|| {
                let mut sends = Vec::new();
                if let Some(prepared) = prepared {
                    match prepared {
                        Ok((replies, received)) => {
                            sends.extend(replies);
                            if let Some(received) = received {
                                if let Err(error) = session.accept(received, &observed_clock) {
                                    tracing::debug!(%error, "compact checkpoint packet rejected");
                                }
                            }
                        }
                        Err(error) => tracing::debug!(%error, "compact checkpoint replay deferred"),
                    }
                }
                if let Some(incoming) = incoming {
                    match session.receive(incoming, now, &observed_clock) {
                        Ok(replies) => sends.extend(replies),
                        Err(error) => tracing::debug!(%error, "checkpoint packet rejected"),
                    }
                }
                sends.extend(session.poll(&peers, Instant::now(), &observed_clock)?);
                Ok::<_, StoreError>(sends)
            })();
            (session, result)
        })
        .await
        .map_err(|error| StoreError::Corruption(format!("checkpoint worker failed: {error}")))?;
        session = returned;
        match result {
            Ok(sends) => {
                let registry = registry.read().await;
                for send in sends {
                    let bytes = WireMessage::CheckpointAgreement(send.packet)
                        .encode()
                        .map_err(|error| StoreError::Corruption(error.to_string()))?;
                    registry.send(&send.peer, bytes);
                }
            }
            Err(error) => tracing::debug!(%error, "checkpoint message or replay deferred"),
        }
    }
}
