//! Ascending account-chain requests and responses.
//!
//! Request IDs correlate replies; they do not authenticate the returned history.
//! The receiver must check chain continuity and admit blocks through validation.

use burst_types::{BlockHash, WalletAddress};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum BootstrapMessage {
    /// Request blocks, account information or a frontier page.
    AscPullReq {
        /// Correlates the [`BootstrapMessage::AscPullAck`] to this request.
        id: u64,
        /// What to pull.
        payload: AscPullReqPayload,
    },
    /// Ascending-bootstrap pull response, echoing the request `id`.
    AscPullAck {
        /// Echoes the request id.
        id: u64,
        /// The pulled data.
        payload: AscPullAckPayload,
    },
}

/// Data requested by [`BootstrapMessage::AscPullReq`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AscPullReqPayload {
    /// Up to `count` blocks for `account`, starting at the block AFTER `start`
    /// (`start` is the requester's current frontier, or `BlockHash::ZERO`
    /// to start from the open block). The server may return fewer blocks
    /// because of frame limits or unavailable history.
    Blocks {
        /// Account whose chain to pull.
        account: WalletAddress,
        /// Requester's current frontier (pull begins at its successor).
        start: BlockHash,
        /// Max blocks to return (server also caps at `ASC_PULL_MAX_BLOCKS`).
        count: u16,
    },
    /// The account's current frontier head and block count.
    AccountInfo {
        /// Account to describe.
        account: WalletAddress,
    },
    /// A page of up to `count` account frontiers from `start_account`.
    Frontiers {
        /// First account to return (inclusive).
        start_account: WalletAddress,
        /// Max frontier entries to return.
        count: u16,
    },
}

/// The data returned in an [`BootstrapMessage::AscPullAck`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AscPullAckPayload {
    /// A contiguous, ascending run of serialized blocks (bincode-encoded).
    Blocks(Vec<Vec<u8>>),
    /// Account frontier head + height (`ZERO`/`0` if the account is unknown).
    AccountInfo {
        /// The account described.
        account: WalletAddress,
        /// Its frontier head (`BlockHash::ZERO` if unknown).
        head: BlockHash,
        /// Its block count (`0` if unknown).
        block_count: u64,
    },
    /// A page of `(account, head)` frontiers.
    Frontiers(Vec<(WalletAddress, BlockHash)>),
}
