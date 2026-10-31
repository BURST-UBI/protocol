//! Pull-based agreement-message delivery. Transport hashes detect mixed bytes;
//! only the agreement driver authenticates votes and replays proposed history.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use bincode::Options;
use burst_consensus::checkpoint::{Checkpoint, CheckpointAnchor};
use burst_crypto::blake2b_256;
use burst_store::StoreError;
use burst_types::BlockHash;
#[cfg(test)]
use burst_types::NetworkId;

use crate::checkpoint_driver::Message;

/// The caller must already have replayed the decision. This builds only its
/// bounded relay cache; it cannot resume voting or admit a new checkpoint.
pub(crate) fn decision_relay(
    parent: &CheckpointAnchor,
    certificate: burst_consensus::checkpoint::CheckpointCertificate,
    manifest: crate::checkpoint_agreement::ReplayManifest,
    now: Instant,
) -> Result<(Context, Transport), StoreError> {
    parent.verify_certificate(&certificate).map_err(invalid)?;
    if manifest.checkpoint() != &certificate.checkpoint {
        return Err(invalid("decision relay manifest differs from certificate"));
    }
    let context = crate::checkpoint_transport::context_from_anchor(parent)?;
    let mut transport = Transport::new(context, now);
    transport.publish(&Message::Decision {
        certificate,
        manifest,
    })?;
    Ok((context, transport))
}

use burst_messages::checkpoint_transport::CHUNK;
use burst_messages::checkpoint_transport::MAX_MESSAGE;
const MAX_CACHE_BYTES: usize = 32 * 1024 * 1024;
// With timely polling and no new messages, cycle a full cache in four seconds.
// Small caches and chunk retries keep their one-second backoff.
const MIN_ANNOUNCE_INTERVAL: Duration = Duration::from_millis(250);
const RETRY_INTERVAL: Duration = Duration::from_secs(1);
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(120);

pub use burst_messages::checkpoint_transport::{Context, Packet, Body};

    pub fn context_from_anchor(anchor: &CheckpointAnchor) -> Result<Context, StoreError> {
        Ok(Context {
            network: anchor.network(),
            genesis: anchor.genesis_hash(),
            parent: anchor.hash(),
            height: anchor
                .height()
                .checked_add(1)
                .ok_or_else(|| invalid("checkpoint height exhausted"))?,
        })
    }
trait ContextMatches {
    fn matches_checkpoint(&self, checkpoint: &Checkpoint) -> bool;
    fn matches(&self, message: &Message) -> bool;
}
impl ContextMatches for Context {


    fn matches_checkpoint(&self, checkpoint: &Checkpoint) -> bool {
        self.network == checkpoint.network
            && self.genesis == checkpoint.genesis
            && self.parent == checkpoint.parent
            && self.height == checkpoint.height
    }

    fn matches(&self, message: &Message) -> bool {
        let ballot = |vote: &burst_consensus::checkpoint_round::SignedBallot| {
            self.network == vote.ballot.network
                && self.genesis == vote.ballot.genesis
                && self.parent == vote.ballot.parent
                && self.height == vote.ballot.height
        };
        match message {
            Message::Proposal { proposal, manifest }
            | Message::Prepared {
                proposal, manifest, ..
            } => {
                self.matches_checkpoint(&proposal.body.checkpoint)
                    && &proposal.body.checkpoint == manifest.checkpoint()
            }
            Message::Decision {
                certificate,
                manifest,
            } => {
                self.matches_checkpoint(&certificate.checkpoint)
                    && &certificate.checkpoint == manifest.checkpoint()
            }
            Message::Vote(vote) => ballot(vote),
            Message::CatchUp { evidence, .. } => {
                !evidence.is_empty() && evidence.iter().all(ballot)
            }
        }
    }
}
pub struct Send {
    pub peer: String,
    pub packet: Packet,
}

pub struct Incoming {
    pub peer: String,
    pub packet: Packet,
}

pub struct Received {
    pub digest: BlockHash,
    pub message: Message,
}

struct Published {
    digest: BlockHash,
    bytes: Vec<u8>,
    announced: bool,
}

struct Offered {
    peer: String,
    digest: BlockHash,
    total: u32,
    offered_at: Instant,
}

struct Download {
    offer: Offered,
    bytes: Vec<u8>,
    started_at: Instant,
    last_progress_at: Instant,
    last_request_at: Instant,
}

pub struct Transport {
    context: Context,
    published: VecDeque<Published>,
    published_bytes: usize,
    offers: VecDeque<Offered>,
    download: Option<Download>,
    accepted: VecDeque<BlockHash>,
    announce_cursor: usize,
    next_announce: Instant,
    chunks_served: usize,
    serve_window_start: Instant,
}

fn invalid(message: impl ToString) -> StoreError {
    StoreError::Corruption(message.to_string())
}

impl Transport {
    pub fn new(context: Context, now: Instant) -> Self {
        Self {
            context,
            published: VecDeque::new(),
            published_bytes: 0,
            offers: VecDeque::new(),
            download: None,
            accepted: VecDeque::new(),
            announce_cursor: 0,
            next_announce: now,
            chunks_served: 0,
            serve_window_start: now,
        }
    }

    /// Call only for driver-approved outgoing messages. Eviction affects delivery
    /// caching, never the durable signer or accepted certificate chain.
    pub fn publish(&mut self, message: &Message) -> Result<BlockHash, StoreError> {
        if !self.context.matches(message)
            || bincode::serialized_size(message).map_err(invalid)? > MAX_MESSAGE as u64
        {
            return Err(invalid(
                "agreement message has wrong context or exceeds limit",
            ));
        }
        let bytes = bincode::serialize(message).map_err(invalid)?;
        let digest = BlockHash::new(blake2b_256(&bytes));
        if self.published.iter().any(|entry| entry.digest == digest) {
            return Ok(digest);
        }
        while self.published.len() >= 16 || self.published_bytes + bytes.len() > MAX_CACHE_BYTES {
            self.published_bytes -= self
                .published
                .pop_front()
                .expect("nonempty full cache")
                .bytes
                .len();
        }
        self.published_bytes += bytes.len();
        self.published.push_back(Published { digest, bytes, announced: false });
        Ok(digest)
    }

    /// Remember driver acceptance to suppress offers until this entry is evicted.
    /// Do not acknowledge replay failures; missing history may become available later.
    pub fn acknowledge(&mut self, digest: BlockHash) {
        if !self.accepted.contains(&digest) {
            if self.accepted.len() == 256 {
                self.accepted.pop_front();
            }
            self.accepted.push_back(digest);
        }
        self.offers.retain(|offer| offer.digest != digest);
    }

    pub fn handle(
        &mut self,
        peer: &str,
        packet: Packet,
        now: Instant,
    ) -> Result<(Option<Send>, Option<Received>), StoreError> {
        if packet.context != self.context || !packet.is_bounded() {
            return Err(invalid("invalid agreement packet context or bounds"));
        }
        match packet.body {
            Body::Offer { digest, total } => {
                let known = self.accepted.contains(&digest)
                    || self.published.iter().any(|entry| entry.digest == digest)
                    || self.download.as_ref().is_some_and(|download| {
                        download.offer.digest == digest && download.offer.peer == peer
                    })
                    || self
                        .offers
                        .iter()
                        .any(|offer| offer.digest == digest && offer.peer == peer);
                if !known
                    && self.offers.len() < 64
                    && self
                        .offers
                        .iter()
                        .filter(|offer| offer.peer == peer)
                        .count()
                        < 4
                {
                    self.offers.push_back(Offered {
                        peer: peer.to_owned(),
                        digest,
                        total,
                        offered_at: now,
                    });
                }
            }
            Body::Request { digest, offset } => {
                if now.saturating_duration_since(self.serve_window_start) >= RETRY_INTERVAL {
                    self.chunks_served = 0;
                    self.serve_window_start = now;
                }
                if self.chunks_served >= 64 {
                    return Ok((None, None));
                }
                if let Some(entry) = self.published.iter().find(|entry| entry.digest == digest) {
                    let start = offset as usize;
                    if start >= entry.bytes.len() {
                        return Err(invalid("agreement request exceeds message"));
                    }
                    self.chunks_served += 1;
                    return Ok((
                        Some(Send {
                            peer: peer.to_owned(),
                            packet: Packet {
                                context: self.context,
                                body: Body::Chunk {
                                    digest,
                                    offset,
                                    total: entry.bytes.len() as u32,
                                    data: entry.bytes
                                        [start..(start + CHUNK).min(entry.bytes.len())]
                                        .to_vec(),
                                },
                            },
                        }),
                        None,
                    ));
                }
            }
            Body::Chunk {
                digest,
                total,
                offset,
                data,
            } => {
                let download = self
                    .download
                    .as_mut()
                    .ok_or_else(|| invalid("unsolicited agreement chunk"))?;
                if download.offer.peer != peer
                    || download.offer.digest != digest
                    || download.offer.total != total
                    || download.bytes.len() != offset as usize
                {
                    return Err(invalid("agreement chunk does not match requested bytes"));
                }
                download.bytes.extend_from_slice(&data);
                download.last_progress_at = now;
                if download.bytes.len() == total as usize {
                    let download = self.download.take().expect("active download");
                    if BlockHash::new(blake2b_256(&download.bytes)) != digest {
                        return Err(invalid("agreement digest mismatch"));
                    }
                    let message = bincode::DefaultOptions::new()
                        .with_fixint_encoding()
                        .reject_trailing_bytes()
                        .with_limit(MAX_MESSAGE as u64)
                        .deserialize(&download.bytes)
                        .map_err(invalid)?;
                    if !self.context.matches(&message) {
                        return Err(invalid("agreement payload context mismatch"));
                    }
                    return Ok((None, Some(Received { digest, message })));
                }
                return Ok((self.request(now), None));
            }
        }
        Ok((None, None))
    }

    fn request(&mut self, now: Instant) -> Option<Send> {
        let download = self.download.as_mut()?;
        download.last_request_at = now;
        Some(Send {
            peer: download.offer.peer.clone(),
            packet: Packet {
                context: self.context,
                body: Body::Request {
                    digest: download.offer.digest,
                    offset: download.bytes.len() as u32,
                },
            },
        })
    }

    pub fn poll(&mut self, peers: &[String], now: Instant) -> Vec<Send> {
        let mut sends = Vec::new();
        if now >= self.next_announce && !self.published.is_empty() {
            // New approved votes should not wait for a complete rebroadcast
            // cycle of older messages. Keep the same bounded cache and one-offer
            // poll; duplicates do not regain first-announcement priority.
            let index = self.published.iter().position(|entry| !entry.announced)
                .unwrap_or_else(|| {
                    let index = self.announce_cursor % self.published.len();
                    self.announce_cursor = (index + 1) % self.published.len();
                    index
                });
            self.published[index].announced = true;
            let entry = &self.published[index];
            let interval = match self.published.len() {
                0..=4 => RETRY_INTERVAL,
                5..=8 => RETRY_INTERVAL / 2,
                _ => MIN_ANNOUNCE_INTERVAL,
            };
            self.next_announce = now + interval;
            for peer in peers.iter().take(128) {
                sends.push(Send {
                    peer: peer.clone(),
                    packet: Packet {
                        context: self.context,
                        body: Body::Offer {
                            digest: entry.digest,
                            total: entry.bytes.len() as u32,
                        },
                    },
                });
            }
        }
        self.offers.retain(|offer| {
            now.saturating_duration_since(offer.offered_at) < TRANSFER_TIMEOUT && peers.contains(&offer.peer)
        });
        if self.download.as_ref().is_some_and(|download| {
            now.saturating_duration_since(download.last_progress_at) >= IDLE_TIMEOUT
                || now.saturating_duration_since(download.started_at) >= TRANSFER_TIMEOUT
                || !peers.contains(&download.offer.peer)
        }) {
            let stalled = self.download.take().expect("expired download");
            self.offers.retain(|offer| offer.peer != stalled.offer.peer);
        }
        if self.download.is_none() {
            if let Some(offer) = self.offers.pop_front() {
                self.download = Some(Download {
                    offer,
                    bytes: Vec::new(),
                    started_at: now,
                    last_progress_at: now,
                    last_request_at: now,
                });
                sends.extend(self.request(now));
            }
        } else if self
            .download
            .as_ref()
            .is_some_and(|download| now.saturating_duration_since(download.last_request_at) >= RETRY_INTERVAL)
        {
            sends.extend(self.request(now));
        }
        sends
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire_message::WireMessage;
    use burst_consensus::{
        checkpoint::{CheckpointSignature, HumanDelegation, HumanRoster},
        checkpoint_round::{Ballot, Phase, SignedBallot},
    };

    fn fixture() -> (Context, Message) {
        let key = burst_crypto::keypair_from_seed(&[1; 32]);
        let address = burst_crypto::derive_address(&key.public);
        let anchor = CheckpointAnchor::genesis(
            NetworkId::Dev,
            BlockHash::new([9; 32]),
            0,
            HumanRoster::new(vec![HumanDelegation {
                human: address.clone(),
                representative: address.clone(),
            }])
            .unwrap(),
            6700,
        )
        .unwrap();
        let context = crate::checkpoint_transport::context_from_anchor(&anchor).unwrap();
        let ballot = Ballot {
            network: context.network,
            genesis: context.genesis,
            parent: context.parent,
            height: context.height,
            round: 1,
            phase: Phase::Prevote,
            value: None,
        };
        let signer = CheckpointSignature {
            representative: address,
            signature: burst_crypto::sign_message(&ballot.signing_payload(), &key.private),
        };
        (context, Message::Vote(SignedBallot { ballot, signer }))
    }

    fn wire(packet: Packet) -> Packet {
        let bytes = WireMessage::CheckpointAgreement(packet)
            .encode_framed()
            .unwrap();
        assert!(bytes.len() < burst_types::MAX_P2P_MESSAGE_BYTES);
        let (WireMessage::CheckpointAgreement(packet), _): (WireMessage, _) =
            WireMessage::decode_framed(&bytes).unwrap()
        else {
            panic!("agreement packet")
        };
        packet
    }

    #[test]
    fn full_cache_is_advertised_within_the_initial_proposal_window() {
        let (context, Message::Vote(mut vote)) = fixture() else {
            unreachable!()
        };
        let now = Instant::now();
        let mut sender = Transport::new(context, now);
        let mut expected = Vec::new();
        for round in 0..16 {
            vote.ballot.round = round;
            vote.signer.signature = burst_crypto::sign_message(
                &vote.ballot.signing_payload(),
                &burst_crypto::keypair_from_seed(&[1; 32]).private,
            );
            expected.push(sender.publish(&Message::Vote(vote.clone())).unwrap());
        }
        let peers = ["receiver".into()];
        let mut advertised = Vec::new();
        // Match the worker's 250 ms timer, without sleeping or doing disk I/O.
        for tick in 0..16 {
            let at = now + Duration::from_millis(tick * 250);
            for send in sender.poll(&peers, at) {
                let Body::Offer { digest, .. } = send.packet.body else {
                    panic!("expected an offer")
                };
                advertised.push(digest);
            }
            assert!(sender.poll(&peers, at).is_empty());
        }
        assert_eq!(advertised, expected);
        // A delayed worker emits one offer, not a burst of missed timer ticks.
        assert_eq!(sender.poll(&peers, now + TRANSFER_TIMEOUT).len(), 1);
        assert!(sender.poll(&peers, now + TRANSFER_TIMEOUT).is_empty());
    }

    #[test]
    fn new_vote_precedes_rebroadcast_without_duplicate_priority_or_starvation() {
        let (context, Message::Vote(mut vote)) = fixture() else { unreachable!() };
        let now = Instant::now();
        let mut sender = Transport::new(context, now);
        let peers = ["receiver".into()];
        let mut messages = Vec::new();
        let mut digests = Vec::new();
        for round in 0..3 {
            vote.ballot.round = round;
            vote.signer.signature = burst_crypto::sign_message(
                &vote.ballot.signing_payload(),
                &burst_crypto::keypair_from_seed(&[1; 32]).private,
            );
            messages.push(Message::Vote(vote.clone()));
            digests.push(sender.publish(messages.last().unwrap()).unwrap());
        }
        let offered = |sends: Vec<Send>| {
            assert_eq!(sends.len(), 1);
            let Body::Offer { digest, .. } = sends[0].packet.body else { panic!("offer") };
            digest
        };
        for second in 0..3 {
            assert_eq!(offered(sender.poll(&peers, now + RETRY_INTERVAL * second)), digests[second as usize]);
        }
        assert_eq!(offered(sender.poll(&peers, now + RETRY_INTERVAL * 3)), digests[0]);
        vote.ballot.round = 3;
        vote.signer.signature = burst_crypto::sign_message(
            &vote.ballot.signing_payload(),
            &burst_crypto::keypair_from_seed(&[1; 32]).private,
        );
        let fresh = sender.publish(&Message::Vote(vote)).unwrap();
        sender.publish(&messages[0]).unwrap();
        // Offer the fresh vote at the next slot, ahead of cached rebroadcasts.
        assert_eq!(offered(sender.poll(&peers, now + RETRY_INTERVAL * 4)), fresh);
        assert!(sender.poll(&peers, now + RETRY_INTERVAL * 4).is_empty());
        let replayed: Vec<_> = (5..9).map(|second|
            offered(sender.poll(&peers, now + RETRY_INTERVAL * second))).collect();
        for digest in digests.iter().chain(std::iter::once(&fresh)) {
            assert_eq!(replayed.iter().filter(|value| *value == digest).count(), 1);
        }
    }

    #[test]
    fn small_cache_does_not_advertise_on_every_worker_tick() {
        let (context, message) = fixture();
        let now = Instant::now();
        let mut sender = Transport::new(context, now);
        sender.publish(&message).unwrap();
        let peers = ["receiver".into()];
        assert_eq!(sender.poll(&peers, now).len(), 1);
        for tick in 1..4 {
            assert!(sender.poll(&peers, now + MIN_ANNOUNCE_INTERVAL * tick).is_empty());
        }
        assert_eq!(sender.poll(&peers, now + RETRY_INTERVAL).len(), 1);
    }

    #[test]
    fn large_messages_cross_small_frames_and_lost_chunks_are_retried() {
        let (context, vote) = fixture();
        let Message::Vote(ballot) = vote else {
            unreachable!()
        };
        // Repeated signatures exercise byte transport only, not quorum admission.
        let message = Message::CatchUp {
            round: 1,
            evidence: vec![ballot; 1000],
        };
        let expected = bincode::serialize(&message).unwrap();
        assert!(expected.len() > burst_types::MAX_P2P_MESSAGE_BYTES);
        let now = Instant::now();
        let mut sender = Transport::new(context, now);
        let digest = sender.publish(&message).unwrap();
        let mut receiver = Transport::new(context, now);
        let offer = sender.poll(&["receiver".into()], now).pop().unwrap();
        receiver.handle("sender", wire(offer.packet), now).unwrap();
        let request = receiver.poll(&["sender".into()], now).pop().unwrap();
        let (lost, _) = sender
            .handle("receiver", wire(request.packet), now)
            .unwrap();
        assert!(lost.is_some());
        assert!(receiver.poll(&["sender".into()], now).is_empty());
        assert!(receiver.poll(&["sender".into()], now + MIN_ANNOUNCE_INTERVAL).is_empty());
        let mut request = receiver
            .poll(&["sender".into()], now + RETRY_INTERVAL)
            .pop()
            .unwrap();
        let mut delivered = None;
        for index in 0..64 {
            let at = now + RETRY_INTERVAL + Duration::from_millis(index);
            let (chunk, _) = sender.handle("receiver", wire(request.packet), at).unwrap();
            let (next, complete) = receiver
                .handle("sender", wire(chunk.unwrap().packet), at)
                .unwrap();
            if let Some(complete) = complete {
                delivered = Some(complete);
                break;
            }
            request = next.unwrap();
        }
        let delivered = delivered.unwrap();
        assert_eq!(delivered.digest, digest);
        assert_eq!(bincode::serialize(&delivered.message).unwrap(), expected);
        // A data-availability/replay failure is not acknowledged; it can retry.
        let offer = sender
            .poll(&["receiver".into()], now + RETRY_INTERVAL * 2)
            .pop()
            .unwrap();
        receiver
            .handle("sender", offer.packet.clone(), now + RETRY_INTERVAL * 2)
            .unwrap();
        assert_eq!(receiver.offers.len(), 1);
        receiver.acknowledge(digest);
        receiver
            .handle("sender", offer.packet, now + RETRY_INTERVAL * 3)
            .unwrap();
        assert!(receiver.offers.is_empty());
    }

    #[test]
    fn offers_are_bounded_and_stalled_peers_do_not_hold_the_download_forever() {
        let (context, _) = fixture();
        let now = Instant::now();
        let mut receiver = Transport::new(context, now);
        let offer = |index: u8| Packet {
            context,
            body: Body::Offer {
                digest: BlockHash::new([index; 32]),
                total: MAX_MESSAGE as u32,
            },
        };
        for index in 1..=100 {
            receiver.handle("stalled", offer(index), now).unwrap();
        }
        assert_eq!(receiver.offers.len(), 4);
        receiver.handle("ready", offer(101), now).unwrap();
        let peers = ["stalled".into(), "ready".into()];
        let request = receiver.poll(&peers, now).pop().unwrap();
        assert_eq!(request.peer, "stalled");
        assert_eq!(receiver.download.as_ref().unwrap().bytes.capacity(), 0);
        // A connected peer that stops answering must not consume another full
        // timeout for every offer it queued ahead of a responsive peer.
        let request = receiver.poll(&peers, now + IDLE_TIMEOUT).pop().unwrap();
        assert_eq!(request.peer, "ready");
        assert!(receiver.offers.is_empty());
        let delayed = Packet {
            context,
            body: Body::Chunk {
                digest: BlockHash::new([1; 32]),
                total: MAX_MESSAGE as u32,
                offset: 0,
                data: vec![0; CHUNK],
            },
        };
        assert!(receiver.handle("stalled", delayed, now + IDLE_TIMEOUT).is_err());
        assert_eq!(receiver.download.as_ref().unwrap().offer.peer, "ready");
        // A timeout is not a ban: a recovered peer can offer data again at the
        // back of the queue, without interrupting the active responsive peer.
        receiver.handle("stalled", offer(1), now + IDLE_TIMEOUT).unwrap();
        assert_eq!(receiver.offers.len(), 1);
        for index in 1..=100 {
            receiver
                .handle(&format!("peer{index}"), offer(index), now + IDLE_TIMEOUT)
                .unwrap();
        }
        assert_eq!(receiver.offers.len(), 64);
        receiver.poll(&[], now + TRANSFER_TIMEOUT);
        assert!(receiver.offers.is_empty());
        assert!(receiver.download.is_none());
    }

    #[test]
    fn wrong_context_unsolicited_corrupt_and_oversized_chunks_are_rejected() {
        let (context, message) = fixture();
        let now = Instant::now();
        let mut sender = Transport::new(context, now);
        sender.publish(&message).unwrap();
        let mut receiver = Transport::new(context, now);
        let offer = sender.poll(&["receiver".into()], now).pop().unwrap().packet;
        let mut wrong = offer.clone();
        wrong.context.height += 1;
        assert!(receiver.handle("sender", wrong, now).is_err());
        assert!(receiver.offers.is_empty());
        receiver.handle("sender", offer, now).unwrap();
        let request = receiver.poll(&["sender".into()], now).pop().unwrap();
        let (chunk, _) = sender.handle("receiver", request.packet, now).unwrap();
        let chunk = chunk.unwrap().packet;
        assert!(receiver.handle("impostor", chunk.clone(), now).is_err());
        assert!(receiver.download.as_ref().unwrap().bytes.is_empty());
        let mut oversize = chunk.clone();
        if let Body::Chunk { total, .. } = &mut oversize.body {
            *total = MAX_MESSAGE as u32 + 1;
        }
        assert!(receiver.handle("sender", oversize, now).is_err());
        let mut corrupt = chunk;
        if let Body::Chunk { data, .. } = &mut corrupt.body {
            data[0] ^= 1;
        }
        assert!(receiver.handle("sender", corrupt.clone(), now).is_err());
        assert!(receiver.download.is_none());
        assert!(receiver.handle("sender", corrupt, now).is_err());
    }
}
