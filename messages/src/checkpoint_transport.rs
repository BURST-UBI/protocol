//! Chunked checkpoint-agreement messages. The node authenticates the assembled
//! message and checks its context against the active agreement round.
use burst_types::{BlockHash, NetworkId};
use serde::{Deserialize, Serialize};

pub const CHUNK: usize = 16 * 1024;
pub const MAX_MESSAGE: usize = 16 * 1024 * 1024;

/// Network and parent checkpoint identifying the intended agreement round.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    pub network: NetworkId,
    pub genesis: BlockHash,
    pub parent: BlockHash,
    pub height: u64,
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
    /// Check nonzero identifiers, the 16 MiB transfer cap and chunk framing.
    /// This does not match the local round, verify the digest or check signatures.
    pub fn is_bounded(&self) -> bool {
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
