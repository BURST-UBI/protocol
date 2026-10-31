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
use burst_types::{BlockHash, NetworkId};
use serde::{Deserialize, Serialize};

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
    let context = Context::from_anchor(parent)?;
    let mut transport = Transport::new(context, now);
    transport.publish(&Message::Decision {
        certificate,
        manifest,
    })?;
    Ok((context, transport))
}

const CHUNK: usize = 16 * 1024;
const MAX_MESSAGE: usize = 16 * 1024 * 1024;
const CACHE_BYTES: usize = 32 * 1024 * 1024;
// Cycle a full cache in four seconds, within the initial proposal window.
// Small caches and chunk retries keep their one-second backoff.
const ANNOUNCE: Duration = Duration::from_millis(250);
const RETRY: Duration = Duration::from_secs(1);
const IDLE: Duration = Duration::from_secs(5);
const LIFETIME: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    pub network: NetworkId,
    pub genesis: BlockHash,
    pub parent: BlockHash,
    pub height: u64,
}

impl Context {
    pub fn from_anchor(anchor: &CheckpointAnchor) -> Result<Self, StoreError> {
        Ok(Self {
            network: anchor.network(),
            genesis: anchor.genesis_hash(),
            parent: anchor.hash(),
            height: anchor
                .height()
                .checked_add(1)
                .ok_or_else(|| invalid("checkpoint height exhausted"))?,
        })
    }

    fn checkpoint(&self, checkpoint: &Checkpoint) -> bool {
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
                self.checkpoint(&proposal.body.checkpoint)
                    && &proposal.body.checkpoint == manifest.checkpoint()
            }
            Message::Decision {
                certificate,
                manifest,
            } => {
                self.checkpoint(&certificate.checkpoint)
                    && &certificate.checkpoint == manifest.checkpoint()
            }
            Message::Vote(vote) => ballot(vote),
            Message::CatchUp { evidence, .. } => {
                !evidence.is_empty() && evidence.iter().all(ballot)
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Packet {
    pub context: Context,
    pub body: Body,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Body {
    Offer {
        digest: BlockHash,
        total: u32,
    },
    Request {
        digest: BlockHash,
        offset: u32,
    },
    Chunk {
        digest: BlockHash,
        total: u32,
        offset: u32,
        data: Vec<u8>,
    },
}

impl Packet {
    pub(crate) fn is_bounded(&self) -> bool {
        if self.context.height == 0
            || self.context.genesis.is_zero()
            || self.context.parent.is_zero()
        {
            return false;
        }
        match &self.body {
            Body::Offer { digest, total } => {
                !digest.is_zero() && (1..=MAX_MESSAGE).contains(&(*total as usize))
            }
            Body::Request { digest, offset } => {
                !digest.is_zero()
                    && (*offset as usize) < MAX_MESSAGE
                    && (*offset as usize).is_multiple_of(CHUNK)
            }
            Body::Chunk {
                digest,
                total,
                offset,
                data,
            } => {
                !digest.is_zero()
                    && (1..=MAX_MESSAGE).contains(&(*total as usize))
                    && offset < total
                    && (*offset as usize).is_multiple_of(CHUNK)
                    && data.len() == CHUNK.min((*total - *offset) as usize)
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
}

struct Offered {
    peer: String,
    digest: BlockHash,
    total: u32,
    offered: Instant,
}

struct Download {
    offer: Offered,
    bytes: Vec<u8>,
    started: Instant,
    progress: Instant,
    requested: Instant,
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
    served: usize,
    serve_window: Instant,
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
            served: 0,
            serve_window: now,
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
        while self.published.len() >= 16 || self.published_bytes + bytes.len() > CACHE_BYTES {
            self.published_bytes -= self
                .published
                .pop_front()
                .expect("nonempty full cache")
                .bytes
                .len();
        }
        self.published_bytes += bytes.len();
        self.published.push_back(Published { digest, bytes });
        Ok(digest)
    }

    /// A digest is suppressed only after the driver accepted the message. Missing
    /// signed history can therefore arrive through normal bootstrap and be retried.
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
                        offered: now,
                    });
                }
            }
            Body::Request { digest, offset } => {
                if now.saturating_duration_since(self.serve_window) >= RETRY {
                    self.served = 0;
                    self.serve_window = now;
                }
                if self.served >= 64 {
                    return Ok((None, None));
                }
                if let Some(entry) = self.published.iter().find(|entry| entry.digest == digest) {
                    let start = offset as usize;
                    if start >= entry.bytes.len() {
                        return Err(invalid("agreement request exceeds message"));
                    }
                    self.served += 1;
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
                download.progress = now;
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
        download.requested = now;
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
            let entry = &self.published[self.announce_cursor % self.published.len()];
            self.announce_cursor = (self.announce_cursor + 1) % self.published.len();
            let interval = match self.published.len() {
                0..=4 => RETRY,
                5..=8 => RETRY / 2,
                _ => ANNOUNCE,
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
            now.saturating_duration_since(offer.offered) < LIFETIME && peers.contains(&offer.peer)
        });
        if self.download.as_ref().is_some_and(|download| {
            now.saturating_duration_since(download.progress) >= IDLE
                || now.saturating_duration_since(download.started) >= LIFETIME
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
                    started: now,
                    progress: now,
                    requested: now,
                });
                sends.extend(self.request(now));
            }
        } else if self
            .download
            .as_ref()
            .is_some_and(|download| now.saturating_duration_since(download.requested) >= RETRY)
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
        let context = Context::from_anchor(&anchor).unwrap();
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
        assert_eq!(sender.poll(&peers, now + LIFETIME).len(), 1);
        assert!(sender.poll(&peers, now + LIFETIME).is_empty());
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
            assert!(sender.poll(&peers, now + ANNOUNCE * tick).is_empty());
        }
        assert_eq!(sender.poll(&peers, now + RETRY).len(), 1);
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
        assert!(receiver.poll(&["sender".into()], now + ANNOUNCE).is_empty());
        let mut request = receiver
            .poll(&["sender".into()], now + RETRY)
            .pop()
            .unwrap();
        let mut delivered = None;
        for index in 0..64 {
            let at = now + RETRY + Duration::from_millis(index);
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
            .poll(&["receiver".into()], now + RETRY * 2)
            .pop()
            .unwrap();
        receiver
            .handle("sender", offer.packet.clone(), now + RETRY * 2)
            .unwrap();
        assert_eq!(receiver.offers.len(), 1);
        receiver.acknowledge(digest);
        receiver
            .handle("sender", offer.packet, now + RETRY * 3)
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
        let request = receiver.poll(&peers, now + IDLE).pop().unwrap();
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
        assert!(receiver.handle("stalled", delayed, now + IDLE).is_err());
        assert_eq!(receiver.download.as_ref().unwrap().offer.peer, "ready");
        // A timeout is not a ban: a recovered peer can offer data again at the
        // back of the queue, without interrupting the active responsive peer.
        receiver.handle("stalled", offer(1), now + IDLE).unwrap();
        assert_eq!(receiver.offers.len(), 1);
        for index in 1..=100 {
            receiver
                .handle(&format!("peer{index}"), offer(index), now + IDLE)
                .unwrap();
        }
        assert_eq!(receiver.offers.len(), 64);
        receiver.poll(&[], now + LIFETIME);
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
