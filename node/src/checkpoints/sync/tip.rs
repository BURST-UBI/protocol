//! Bounded peer observations, never checkpoint authority or a finality proof.
use super::{CheckpointSyncMessage as Message, Incoming, PEER_TIMEOUT};
use burst_types::BlockHash;
use std::time::{Duration, Instant};

const FRESHNESS: Duration = Duration::from_secs(15);

/// Bound both retained peer identities and replies during each cooldown.
#[derive(Default)]
pub(super) struct ReplyLimit(std::collections::HashMap<String, Instant>);

impl ReplyLimit {
    const CAPACITY: usize = 256;

    pub(super) fn allow(&mut self, peer: &str, now: Instant) -> bool {
        self.0
            .retain(|_, served| now.duration_since(*served) < super::RETRY_INTERVAL);
        if self.0.contains_key(peer) || self.0.len() >= Self::CAPACITY {
            return false;
        }
        self.0.insert(peer.to_owned(), now);
        true
    }
}

#[derive(Default)]
pub(crate) struct Published(std::sync::Mutex<Option<(Instant, serde_json::Value)>>);

impl Published {
    pub(super) fn update(&self, expires: Instant, value: serde_json::Value) {
        if let Ok(mut current) = self.0.lock() {
            if current
                .as_ref()
                .is_none_or(|(_, previous)| previous != &value)
            {
                tracing::debug!(status = %value, "checkpoint signing observation changed");
            }
            *current = Some((expires, value));
        }
    }

    pub(crate) fn read(&self, epoch: BlockHash) -> serde_json::Value {
        if let Ok(current) = self.0.lock() {
            if let Some((expires, value)) = &*current {
                if Instant::now() < *expires && value["payment_epoch"] == epoch.to_string() {
                    return value.clone();
                }
            }
        }
        serde_json::json!({"phase":"unknown", "payment_epoch":epoch.to_string(),
            "observed_epoch":null, "observed_height":null})
    }
}

struct Request {
    peer: String,
    nonce: u64,
    sent: Instant,
}

struct Observation {
    peer: String,
    epoch: BlockHash,
    height: u64,
    received: Instant,
}

pub(super) struct Discovery {
    request: Option<Request>,
    observation: Option<Observation>,
    nonce: u64,
    cursor: usize,
    next: Instant,
}

impl Discovery {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            request: None,
            observation: None,
            nonce: 0,
            cursor: 0,
            next: now,
        }
    }

    pub(super) fn poll(&mut self, peers: &[String], now: Instant) -> Option<Incoming> {
        if self.observation.as_ref().is_some_and(|o| {
            now.duration_since(o.received) >= FRESHNESS || !peers.contains(&o.peer)
        }) {
            self.observation = None;
        }
        if self
            .request
            .as_ref()
            .is_some_and(|r| now.duration_since(r.sent) >= PEER_TIMEOUT || !peers.contains(&r.peer))
        {
            self.request = None;
        }
        if peers.is_empty() || self.request.is_some() || now < self.next {
            return None;
        }
        let mut peers = peers.to_vec();
        peers.sort();
        let peer = peers[self.cursor % peers.len()].clone();
        self.cursor = self.cursor.wrapping_add(1);
        // Exhaustion stops discovery instead of accepting a reused challenge.
        self.nonce = self.nonce.checked_add(1)?;
        self.next = now + PEER_TIMEOUT;
        self.request = Some(Request {
            peer: peer.clone(),
            nonce: self.nonce,
            sent: now,
        });
        Some(Incoming {
            peer,
            message: Message::TipRequest { nonce: self.nonce },
        })
    }

    pub(super) fn receive(
        &mut self,
        peer: &str,
        nonce: u64,
        height: u64,
        epoch: BlockHash,
        now: Instant,
    ) {
        let Some(request) = &self.request else { return };
        if request.peer != peer
            || request.nonce != nonce
            || epoch.is_zero()
            || now.duration_since(request.sent) >= PEER_TIMEOUT
        {
            return;
        }
        self.request = None;
        // Replace, do not accumulate a peer-controlled high-water mark.
        self.observation = Some(Observation {
            peer: peer.into(),
            epoch,
            height,
            received: now,
        });
    }

    pub(super) fn expires(&self, now: Instant) -> Instant {
        self.observation
            .as_ref()
            .map_or(now + FRESHNESS, |o| o.received + FRESHNESS)
    }

    pub(super) fn view(
        &self,
        now: Instant,
        installed: BlockHash,
        height: u64,
        installing: bool,
    ) -> serde_json::Value {
        let observation = self
            .observation
            .as_ref()
            .filter(|o| now.duration_since(o.received) < FRESHNESS);
        let phase = if installing {
            "catching_up"
        } else if observation.is_some_and(|o| o.epoch == installed && o.height == height) {
            "peer_matches"
        } else if observation.is_some() {
            "peer_differs"
        } else {
            "unknown"
        };
        serde_json::json!({
            "phase": phase, "payment_epoch": installed.to_string(),
            "observed_epoch": observation.map(|o| o.epoch.to_string()),
            "observed_height": observation.map(|o| o.height),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_limits_isolate_peers_and_release_bounded_slots() {
        let now = Instant::now();
        let mut limit = ReplyLimit::default();
        for index in 0..ReplyLimit::CAPACITY {
            assert!(limit.allow(&index.to_string(), now));
            assert!(!limit.allow(&index.to_string(), now));
        }
        assert!(!limit.allow("overflow", now));
        assert_eq!(limit.0.len(), ReplyLimit::CAPACITY);
        assert!(limit.allow("overflow", now + super::super::RETRY_INTERVAL));
        assert_eq!(limit.0.len(), 1);
        assert!(limit.allow("0", now + super::super::RETRY_INTERVAL));
    }

    #[test]
    fn published_view_expires_and_cannot_survive_an_installed_epoch_change() {
        let now = Instant::now();
        let epoch = BlockHash::new([1; 32]);
        let published = Published::default();
        assert_eq!(published.read(epoch)["phase"], "unknown");
        let view = serde_json::json!({"phase":"peer_matches", "payment_epoch":epoch.to_string()});
        published.update(now + FRESHNESS, view.clone());
        assert_eq!(published.read(epoch)["phase"], "peer_matches");
        assert_eq!(published.read(BlockHash::new([2; 32]))["phase"], "unknown");
        published.update(now - Duration::from_secs(1), view);
        assert_eq!(published.read(epoch)["phase"], "unknown");
    }

    #[test]
    fn wire_roundtrip_and_bounds() {
        use crate::wire_message::WireMessage;
        for message in [
            Message::TipRequest { nonce: 7 },
            Message::TipObservation {
                nonce: 7,
                height: u64::MAX,
                epoch: BlockHash::new([2; 32]),
            },
        ] {
            let encoded = WireMessage::CheckpointSync(message.clone())
                .encode()
                .unwrap();
            assert!(encoded.len() < 128);
            let WireMessage::CheckpointSync(decoded) = WireMessage::decode(&encoded).unwrap()
            else {
                panic!("wrong envelope")
            };
            assert_eq!(
                serde_json::to_value(decoded).unwrap(),
                serde_json::to_value(message).unwrap()
            );
        }
        assert!(!Message::TipRequest { nonce: 0 }.is_bounded());
        assert!(!Message::TipObservation {
            nonce: 1,
            height: 0,
            epoch: BlockHash::ZERO
        }
        .is_bounded());
    }

    #[test]
    fn matching_reply_is_scoped_to_peer_nonce_and_deadline() {
        let now = Instant::now();
        let epoch = BlockHash::new([1; 32]);
        let mut discovery = Discovery::new(now);
        let peers = vec!["a".into(), "b".into()];
        discovery.poll(&peers, now).unwrap();
        discovery.receive("b", 1, 4, epoch, now);
        discovery.receive("a", 2, 4, epoch, now);
        assert_eq!(discovery.view(now, epoch, 4, false)["phase"], "unknown");
        discovery.receive("a", 1, 4, epoch, now);
        assert_eq!(
            discovery.view(now, epoch, 4, false)["phase"],
            "peer_matches"
        );
        assert_eq!(
            discovery.view(now, epoch, 5, false)["phase"],
            "peer_differs"
        );
        assert_eq!(discovery.view(now, epoch, 4, true)["phase"], "catching_up");
        assert_eq!(
            discovery.view(now, BlockHash::new([2; 32]), 4, false)["phase"],
            "peer_differs"
        );
        assert_eq!(
            discovery.view(now + FRESHNESS, epoch, 4, false)["phase"],
            "unknown"
        );
    }

    #[test]
    fn silence_disconnect_and_large_claims_do_not_pin_discovery() {
        let now = Instant::now();
        let epoch = BlockHash::new([1; 32]);
        let mut discovery = Discovery::new(now);
        let peers = vec!["a".into(), "b".into()];
        assert_eq!(discovery.poll(&peers, now).unwrap().peer, "a");
        let later = now + PEER_TIMEOUT;
        assert_eq!(discovery.poll(&peers, later).unwrap().peer, "b");
        discovery.receive("a", 1, u64::MAX, epoch, later);
        assert_eq!(discovery.view(later, epoch, 4, false)["phase"], "unknown");
        discovery.receive("b", 2, u64::MAX, BlockHash::new([2; 32]), later);
        discovery.poll(&["a".into()], later + PEER_TIMEOUT).unwrap();
        assert_eq!(
            discovery.view(later + PEER_TIMEOUT, epoch, 4, false)["phase"],
            "unknown"
        );
        discovery.receive("a", 3, 4, epoch, later + PEER_TIMEOUT);
        assert_eq!(
            discovery.view(later + PEER_TIMEOUT, epoch, 4, false)["phase"],
            "peer_matches"
        );
        discovery.poll(&[], later + PEER_TIMEOUT);
        assert_eq!(
            discovery.view(later + PEER_TIMEOUT, epoch, 4, false)["phase"],
            "unknown"
        );
    }
}
