//! Request and transfer proposal-content bytes. The receiving service validates
//! the assembled content; receiving a chunk does not admit a proposal.

use burst_governance::content::ContentChunk;
use burst_types::BlockHash;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Packet {
    Request {
        proposal: BlockHash,
        offset: u32,
    },
    Chunk {
        proposal: BlockHash,
        chunk: ContentChunk,
    },
}
