//! Shared immutable historical head nodes. Integrity is not certificate authority.
use crate::LmdbError;
use burst_store::StoreError;
use heed::{types::Bytes, Database, RoTxn};

pub type NodeHash = [u8; 32];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeadNode {
    Leaf {
        account: [u8; 32],
        height: u64,
        hash: [u8; 32],
    },
    Branch {
        bit: u16,
        left: NodeHash,
        right: NodeHash,
    },
}

pub(crate) fn invalid(message: &str) -> StoreError {
    StoreError::Corruption(message.into())
}

impl HeadNode {
    fn validate(&self) -> Result<(), StoreError> {
        match self {
            Self::Leaf { height, hash, .. } if *height == 0 || *hash == [0; 32] => {
                Err(invalid("invalid historical head leaf"))
            }
            Self::Branch { bit, left, right } if *bit >= 256 || left == right => {
                Err(invalid("invalid historical head branch"))
            }
            _ => Ok(()),
        }
    }

    fn encode_into<'a>(&self, bytes: &'a mut [u8; 73]) -> Result<&'a [u8], StoreError> {
        self.validate()?;
        let size = match self {
            Self::Leaf {
                account,
                height,
                hash,
            } => {
                bytes[0] = 0;
                bytes[1..33].copy_from_slice(account);
                bytes[33..41].copy_from_slice(&height.to_be_bytes());
                bytes[41..73].copy_from_slice(hash);
                73
            }
            Self::Branch { bit, left, right } => {
                bytes[0] = 1;
                bytes[1..3].copy_from_slice(&bit.to_be_bytes());
                bytes[3..35].copy_from_slice(left);
                bytes[35..67].copy_from_slice(right);
                67
            }
        };
        Ok(&bytes[..size])
    }

    pub fn encode(&self) -> Result<Vec<u8>, StoreError> {
        Ok(self.encode_into(&mut [0; 73])?.to_vec())
    }

    pub fn digest(&self) -> Result<NodeHash, StoreError> {
        Ok(digest(self.encode_into(&mut [0; 73])?))
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, StoreError> {
        let node = match bytes {
            [0, ..] if bytes.len() == 73 => Self::Leaf {
                account: bytes[1..33].try_into().unwrap(),
                height: u64::from_be_bytes(bytes[33..41].try_into().unwrap()),
                hash: bytes[41..73].try_into().unwrap(),
            },
            [1, ..] if bytes.len() == 67 => Self::Branch {
                bit: u16::from_be_bytes(bytes[1..3].try_into().unwrap()),
                left: bytes[3..35].try_into().unwrap(),
                right: bytes[35..67].try_into().unwrap(),
            },
            _ => return Err(invalid("invalid historical head node encoding")),
        };
        node.validate()?;
        Ok(node)
    }
}

pub(crate) fn digest(bytes: &[u8]) -> NodeHash {
    let mut digest = burst_crypto::Blake2b256Hasher::default();
    digest.update(b"BURST/historical-head-tree/v1\0");
    digest.update(bytes);
    digest.finalize()
}

pub(crate) fn key(kind: u8, hash: &NodeHash) -> [u8; 33] {
    let mut key = [0; 33];
    key[0] = kind;
    key[1..].copy_from_slice(hash);
    key
}

pub(crate) fn read(
    db: Database<Bytes, Bytes>,
    txn: &RoTxn<'_>,
    hash: &NodeHash,
) -> Result<Option<(u64, HeadNode)>, StoreError> {
    let row = db.get(txn, &key(0, hash)).map_err(LmdbError::from)?;
    let queued = db.get(txn, &key(1, hash)).map_err(LmdbError::from)?;
    let Some(bytes) = row else {
        if queued.is_some() {
            return Err(invalid("historical head cleanup references a missing node"));
        }
        return Ok(None);
    };
    if bytes.len() != 81 && bytes.len() != 75 {
        return Err(invalid("invalid historical head row length"));
    }
    let references = u64::from_be_bytes(bytes[..8].try_into().unwrap());
    if queued.is_some_and(|value| !value.is_empty()) || queued.is_some() != (references == 0) {
        return Err(invalid(
            "historical head ownership and cleanup queue disagree",
        ));
    }
    if digest(&bytes[8..]) != *hash {
        return Err(invalid("historical head node digest mismatch"));
    }
    Ok(Some((references, HeadNode::decode(&bytes[8..])?)))
}

pub(crate) const MAX_STAGING_ROOTS: usize = 257;
const STAGING_MAGIC: &[u8; 8] = b"HDSTAGE1";

pub(crate) fn staging_key(id: &NodeHash) -> String {
    format!(
        "checkpoint_heads/staging/v1/{}",
        burst_types::BlockHash::new(*id)
    )
}

pub(crate) fn encode_staging(roots: &[NodeHash]) -> Result<Vec<u8>, StoreError> {
    if roots.len() > MAX_STAGING_ROOTS
        || roots
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != roots.len()
    {
        return Err(invalid("invalid historical head staging roots"));
    }
    let mut bytes = Vec::with_capacity(10 + roots.len() * 32);
    bytes.extend_from_slice(STAGING_MAGIC);
    bytes.extend_from_slice(&(roots.len() as u16).to_be_bytes());
    for root in roots {
        bytes.extend_from_slice(root);
    }
    Ok(bytes)
}

pub(crate) fn read_staging(
    meta: Database<Bytes, Bytes>,
    nodes: Database<Bytes, Bytes>,
    txn: &RoTxn<'_>,
    id: &NodeHash,
) -> Result<Vec<NodeHash>, StoreError> {
    let key = staging_key(id);
    let Some(bytes) = meta.get(txn, key.as_bytes()).map_err(LmdbError::from)? else {
        return Ok(Vec::new());
    };
    if bytes.len() < 10 || &bytes[..8] != STAGING_MAGIC {
        return Err(invalid("invalid historical head staging descriptor"));
    }
    let count = u16::from_be_bytes(bytes[8..10].try_into().unwrap()) as usize;
    if count == 0 || count > MAX_STAGING_ROOTS || bytes.len() != 10 + count * 32 {
        return Err(invalid("invalid historical head staging count"));
    }
    let roots: Vec<NodeHash> = bytes[10..]
        .chunks_exact(32)
        .map(|b| b.try_into().unwrap())
        .collect();
    if roots
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != count
    {
        return Err(invalid("duplicate historical head staging root"));
    }
    for root in &roots {
        if read(nodes, txn, root)?.is_none_or(|(count, _)| count == 0) {
            return Err(invalid("historical head staging root is not owned"));
        }
    }
    Ok(roots)
}

pub(crate) fn next_unowned(
    db: Database<Bytes, Bytes>,
    txn: &RoTxn<'_>,
) -> Result<Option<NodeHash>, StoreError> {
    let mut rows = db.prefix_iter(txn, &[1]).map_err(LmdbError::from)?;
    match rows.next().transpose().map_err(LmdbError::from)? {
        Some((key, value)) if key.len() == 33 && value.is_empty() => {
            Ok(Some(key[1..].try_into().unwrap()))
        }
        Some(_) => Err(invalid("invalid historical head cleanup entry")),
        None => Ok(None),
    }
}

#[derive(Debug)]
pub struct ReclaimedHeads {
    pub nodes: usize,
    pub complete: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_head_codec_preserves_wire_bytes_and_digest_domain() {
        let leaf = HeadNode::Leaf {
            account: [3; 32],
            height: 0x0102030405060708,
            hash: [7; 32],
        };
        let mut leaf_bytes = vec![0];
        leaf_bytes.extend_from_slice(&[3; 32]);
        leaf_bytes.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        leaf_bytes.extend_from_slice(&[7; 32]);
        let branch = HeadNode::Branch {
            bit: 255,
            left: [4; 32],
            right: [9; 32],
        };
        let mut branch_bytes = vec![1, 0, 255];
        branch_bytes.extend_from_slice(&[4; 32]);
        branch_bytes.extend_from_slice(&[9; 32]);
        for (node, bytes) in [(leaf, leaf_bytes), (branch, branch_bytes)] {
            assert_eq!(node.encode().unwrap(), bytes);
            assert_eq!(HeadNode::decode(&bytes).unwrap(), node);
            let mut domain_bytes = b"BURST/historical-head-tree/v1\0".to_vec();
            domain_bytes.extend_from_slice(&bytes);
            assert_eq!(
                node.digest().unwrap(),
                burst_crypto::blake2b_256(&domain_bytes)
            );
            for end in 0..bytes.len() {
                assert!(HeadNode::decode(&bytes[..end]).is_err());
            }
            let mut trailing = bytes;
            trailing.push(0);
            assert!(HeadNode::decode(&trailing).is_err());
        }
        for node in [
            HeadNode::Leaf {
                account: [3; 32],
                height: 0,
                hash: [7; 32],
            },
            HeadNode::Leaf {
                account: [3; 32],
                height: 1,
                hash: [0; 32],
            },
            HeadNode::Branch {
                bit: 256,
                left: [4; 32],
                right: [9; 32],
            },
            HeadNode::Branch {
                bit: 1,
                left: [4; 32],
                right: [4; 32],
            },
        ] {
            assert!(node.encode().is_err());
            assert!(node.digest().is_err());
        }
    }
}
