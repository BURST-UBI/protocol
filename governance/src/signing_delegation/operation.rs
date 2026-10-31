//! Bounded delegation operations carried by primary-signed account blocks.

use super::{DelegatedBallot, Grant, KeyScope, Revocation};
use bincode::Options;
use burst_types::{BlockHash, NetworkId, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

pub const MAX_OPERATION_BYTES: usize = 1024;
pub const LEDGER_DELEGATION_ENABLED: bool = false;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DelegationOperation {
    Grant(Grant),
    Revoke(Revocation),
    /// The recipient's account carries a secondary-signed candidate. Its
    /// explicit grant-block dependency supplies the historical key proof.
    Ballot {
        ballot: DelegatedBallot,
        grant_block: BlockHash,
    },
}

impl DelegationOperation {
    pub fn identity(&self) -> Option<(NetworkId, BlockHash, &WalletAddress, &KeyScope)> {
        match self {
            Self::Grant(value) => Some((value.network, value.genesis, &value.owner, &value.scope)),
            Self::Revoke(value) => Some((value.network, value.genesis, &value.owner, &value.scope)),
            Self::Ballot { .. } => None,
        }
    }

    pub fn proposal(&self) -> Option<TxHash> {
        match self.identity()?.3 {
            KeyScope::Proposal(hash) => Some(*hash),
            _ => None,
        }
    }

    /// Check grant/revocation signatures or a ballot's reference structure.
    /// Ballot signatures need the historical grant and are checked separately.
    /// The containing account block's signer and ledger dependencies are not
    /// authenticated by this method.
    pub fn validate_envelope(&self) -> Result<(), String> {
        match self {
            Self::Grant(value) => value.verify(),
            Self::Revoke(value) => value.verify(),
            Self::Ballot {
                ballot,
                grant_block,
            } => {
                if grant_block.is_zero()
                    || ballot.proposal.is_zero()
                    || ballot.grant.is_zero()
                    || burst_crypto::decode_address(ballot.owner.as_str()).is_none()
                {
                    return Err("invalid delegated ballot references".into());
                }
                Ok(())
            }
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate_envelope()?;
        let bytes = bincode::serialize(self).map_err(|error| error.to_string())?;
        if bytes.len() > MAX_OPERATION_BYTES {
            return Err("delegation operation is oversized".into());
        }
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_OPERATION_BYTES {
            return Err("delegation operation is oversized".into());
        }
        let value: Self = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(MAX_OPERATION_BYTES as u64)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map_err(|error| error.to_string())?;
        value.validate_envelope()?;
        Ok(value)
    }
}
