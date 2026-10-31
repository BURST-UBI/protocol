use super::*;
use crate::{shutdown::ShutdownController, wire_message::WireMessage};
use tokio::sync::mpsc;

/// Proof transport has no signing key, admission queue or checkpoint driver.
/// It can start before ledger recovery and remain running through handoff.
pub(crate) async fn start(
    store: Arc<LmdbStore>,
    registry: Arc<RwLock<ConnectionRegistry>>,
    shutdown: Arc<ShutdownController>,
) {
    let mut connections = registry.write().await;
    if connections
        .witness_requests()
        .is_some_and(|sender| !sender.is_closed())
    {
        return;
    }
    let (commands, command_rx) = mpsc::channel(1);
    let (incoming, inbox) = mpsc::channel(crate::checkpoint_sync::INBOX_CAPACITY);
    connections.set_witness_requests(commands);
    connections.set_witness_inbox(incoming);
    let stop = shutdown.subscribe();
    let worker = tokio::spawn(run(
        store,
        Arc::clone(&registry),
        shutdown,
        stop,
        command_rx,
        inbox,
    ));
    connections.track_task(worker);
}

#[cfg(test)]
mod tests;

async fn run(
    store: Arc<LmdbStore>,
    registry: Arc<RwLock<ConnectionRegistry>>,
    shutdown: Arc<ShutdownController>,
    mut stop: tokio::sync::broadcast::Receiver<()>,
    mut commands: mpsc::Receiver<Command>,
    mut inbox: mpsc::Receiver<Incoming>,
) {
    let mut transfer = Transfer::new(Instant::now());
    let mut timer = tokio::time::interval(RETRY_INTERVAL);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut scheduler = crate::checkpoint_sync::input::Scheduler::default();
    let mut scheduler_stop = stop.resubscribe();
    loop {
        let incoming = tokio::select! {
            biased;
            _ = stop.recv() => break,
            Some(command) = commands.recv(), if transfer.job.is_none() => {
                if !command.result.is_closed() {
                    let kind = match &command.roots {
                        Roots::Account(_) => "account",
                        Roots::Plan(_) => "plan",
                        Roots::Stored(_) => "stored",
                        Roots::Exact(_) => "exact",
                        Roots::Suffix { .. } => "suffix",
                    };
                    tracing::debug!(kind, wanted = command.wanted.len(), "historical witness recovery started");
                    transfer.start(command, Instant::now());
                    timer.reset_immediately();
                }
                continue;
            }
            result = scheduler.next(&mut scheduler_stop, &mut inbox, &mut timer) => match result {
                Ok(incoming) => incoming,
                Err(()) => break,
            },
        };
        let peers = if incoming.is_none() {
            registry
                .read()
                .await
                .peer_ids()
                .into_iter()
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        let source = Arc::clone(&store);
        let active = transfer.job.is_some();
        let result = tokio::task::spawn_blocking(move || {
            let now = Instant::now();
            let mut local_progress = false;
            let reply = match incoming {
                Some(incoming) => transfer.handle(&source, incoming, now),
                None if transfer.local_progress(&source, now) => {
                    local_progress = true;
                    Ok(None)
                }
                None => Ok(transfer.poll(peers, now)),
            };
            (transfer, reply, local_progress)
        })
        .await;
        let (returned, reply, local_progress) = match result {
            Ok(value) => value,
            Err(error) => {
                shutdown.fail("historical witness worker", &error);
                break;
            }
        };
        transfer = returned;
        if local_progress && transfer.job.is_some() {
            // Local work has already made progress. Do not charge each bounded
            // chunk the peer retry delay; the scheduler still serves shutdown
            // and alternates ready packets with local polls.
            timer.reset_immediately();
        }
        if active && transfer.job.is_none() {
            tracing::debug!("historical witness recovery finished");
        }
        match reply {
            Ok(Some(reply)) => match WireMessage::CheckpointSync(reply.message).encode() {
                Ok(bytes) => {
                    registry.read().await.send(&reply.peer, bytes);
                }
                Err(error) => tracing::error!(%error, "cannot encode historical witness response"),
            },
            Ok(None) => (),
            Err(error) => tracing::debug!(%error, "historical witness transfer deferred"),
        }
    }
}
