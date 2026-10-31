//! Bounded per-socket queues with control capacity and a shared node budget.
use crate::{
    admission::ConnectionPermit,
    bandwidth::{BandwidthLimits, SharedBandwidth, TrafficClass},
};
use std::{sync::Arc, time::Duration};
use tokio::{io::AsyncWriteExt, net::tcp::OwnedWriteHalf, sync::mpsc, task::JoinHandle};

pub const BULK_QUEUE_CAPACITY: usize = 256;
pub const CONTROL_QUEUE_CAPACITY: usize = 64;

pub struct Sender {
    bulk: mpsc::Sender<Vec<u8>>,
    control: mpsc::Sender<Vec<u8>>,
}
impl Sender {
    pub fn try_send(&self, payload: Vec<u8>, class: TrafficClass) -> Result<(), Vec<u8>> {
        if payload.len() > burst_types::MAX_P2P_MESSAGE_BYTES {
            return Err(payload);
        }
        let sender = match class {
            TrafficClass::Control => &self.control,
            TrafficClass::Bulk => &self.bulk,
        };
        sender.try_send(payload).map_err(|error| error.into_inner())
    }
}

/// Start a writer that prefers control when choosing the next queued message.
/// It does not preempt a frame already being paced or written. Queue bounds count
/// messages; one dequeued frame is held separately. The task retains admission
/// until it exits. Each write, including control pacing, has a 30-second deadline.
pub fn spawn(
    mut writer: OwnedWriteHalf,
    shared: Arc<SharedBandwidth>,
    admission: Arc<ConnectionPermit>,
    observe: impl Fn(&[u8], &'static str) + Send + 'static,
) -> (Sender, JoinHandle<()>) {
    let (bulk_tx, mut bulk) = mpsc::channel::<Vec<u8>>(BULK_QUEUE_CAPACITY);
    let (control_tx, mut control) = mpsc::channel::<Vec<u8>>(CONTROL_QUEUE_CAPACITY);
    let task = tokio::spawn(async move {
        let _admission = admission;
        let per_peer = SharedBandwidth::new(&BandwidthLimits {
            total_bytes_per_second: crate::DEFAULT_MAX_BYTES_PER_SEC,
            bulk_bytes_per_second: crate::DEFAULT_MAX_BYTES_PER_SEC * 4 / 5,
        });
        loop {
            let (payload, class) = tokio::select! {
                biased;
                Some(payload) = control.recv() => (payload, TrafficClass::Control),
                Some(payload) = bulk.recv() => (payload, TrafficClass::Bulk),
                else => break,
            };
            observe(&payload, "writer_dequeued");
            // Bulk drops on budget denial. Control waits with one dequeued frame.
            // Budgets are reserved sequentially: a later denial or write failure
            // does not refund credit already spent in the first budget.
            if class == TrafficClass::Bulk
                && (!per_peer.try_consume(payload.len() + 4, class)
                    || !shared.try_consume(payload.len() + 4, class))
            {
                observe(&payload, "writer_throttled");
                continue;
            }
            let write = async {
                if class == TrafficClass::Control {
                    per_peer.acquire(payload.len() + 4, class).await;
                    shared.acquire(payload.len() + 4, class).await;
                }
                writer
                    .write_all(&(payload.len() as u32).to_be_bytes())
                    .await?;
                writer.write_all(&payload).await?;
                writer.flush().await
            };
            if !matches!(
                tokio::time::timeout(Duration::from_secs(30), write).await,
                Ok(Ok(()))
            ) {
                observe(&payload, "writer_failed");
                break;
            }
            observe(&payload, "writer_finished");
        }
    });
    (
        Sender {
            bulk: bulk_tx,
            control: control_tx,
        },
        task,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::{PeerAdmission, PeerLimits};
    use tokio::{
        io::AsyncReadExt,
        net::{TcpListener, TcpStream},
    };

    async fn sockets() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, _) = listener.accept().await.unwrap();
        (client, server)
    }

    #[tokio::test]
    async fn full_bulk_queue_preserves_control_and_socket_lifetime_holds_admission() {
        let (mut client, server) = sockets().await;
        let ip = server.peer_addr().unwrap().ip();
        let limits = PeerAdmission::new(1, PeerLimits::for_test_network(1));
        let permit = limits.acquire(ip, false).unwrap();
        let (sender, task) = spawn(
            server.into_split().1,
            Arc::new(SharedBandwidth::new(&BandwidthLimits::default())),
            permit,
            |_, _| {},
        );
        for _ in 0..BULK_QUEUE_CAPACITY {
            sender.try_send(vec![0; 8], TrafficClass::Bulk).unwrap();
        }
        assert!(sender.try_send(vec![0; 8], TrafficClass::Bulk).is_err());
        sender.try_send(vec![7; 8], TrafficClass::Control).unwrap();
        assert!(sender
            .try_send(
                vec![0; burst_types::MAX_P2P_MESSAGE_BYTES + 1],
                TrafficClass::Control
            )
            .is_err());
        assert!(limits.acquire(ip, false).is_none());
        let mut bytes = [0; 12];
        tokio::time::timeout(Duration::from_secs(2), client.read_exact(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&bytes[..4], &8u32.to_be_bytes());
        assert_eq!(&bytes[4..], &[7; 8]);
        drop(sender);
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert!(limits.acquire(ip, false).is_some());
    }

    #[tokio::test]
    async fn exhausted_shared_budget_delays_control_without_losing_or_reordering_it() {
        let (mut client, server) = sockets().await;
        let limits = PeerAdmission::new(1, PeerLimits::for_test_network(1));
        let budget = Arc::new(SharedBandwidth::new(&BandwidthLimits {
            total_bytes_per_second: 100,
            bulk_bytes_per_second: 80,
        }));
        assert!(budget.try_consume(100, TrafficClass::Control));
        let (sender, task) = spawn(
            server.into_split().1,
            budget,
            limits.acquire("127.0.0.1".parse().unwrap(), false).unwrap(),
            |_, _| {},
        );
        sender.try_send(vec![7; 8], TrafficClass::Control).unwrap();
        sender.try_send(vec![9; 8], TrafficClass::Control).unwrap();
        drop(sender);
        let mut bytes = [0; 24];
        tokio::time::timeout(Duration::from_secs(3), client.read_exact(&mut bytes))
            .await.unwrap().unwrap();
        assert_eq!(&bytes[..4], &8u32.to_be_bytes());
        assert_eq!(&bytes[4..12], &[7; 8]);
        assert_eq!(&bytes[12..16], &8u32.to_be_bytes());
        assert_eq!(&bytes[16..], &[9; 8]);
        tokio::time::timeout(Duration::from_secs(3), task).await.unwrap().unwrap();
        assert!(limits.acquire("127.0.0.1".parse().unwrap(), false).is_some());
    }

    #[tokio::test]
    async fn aborting_writer_returns_connection_capacity() {
        let (_client, server) = sockets().await;
        let ip = server.peer_addr().unwrap().ip();
        let limits = PeerAdmission::new(1, PeerLimits::for_test_network(1));
        let (sender, task) = spawn(
            server.into_split().1,
            Arc::new(SharedBandwidth::new(&BandwidthLimits::default())),
            limits.acquire(ip, false).unwrap(),
            |_, _| {},
        );
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(sender.try_send(vec![0; 8], TrafficClass::Bulk).is_err());
        assert!(limits.acquire(ip, false).is_some());
    }
}
