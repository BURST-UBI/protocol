//! Canonical proposal content and bounded transfer of content too large for one
//! peer frame. A manifest is a commitment, not an authorization: callers must
//! obtain it from a signed proposal before accepting or storing received data.

use crate::ProposalContent;
use bincode::Options;
use burst_types::{BlockHash, TxHash};
use serde::{Deserialize, Serialize};

/// Amendment body limit in Unicode scalar values, not UTF-8 bytes or graphemes.
pub const MAX_AMENDMENT_CHARS: usize = 50_000;
/// Keep ordinary nodes gated until live submission, catch-up and recovery pass.
pub const FULL_CONTENT_ENABLED: bool = false;
/// Includes titles, descriptions and the encoded envelope. A maximal four-byte
/// UTF-8 amendment fits without changing the 64 KiB peer-frame limit.
pub const MAX_CONTENT_BYTES: usize = 512 * 1024;
pub const CONTENT_CHUNK_BYTES: usize = 32 * 1024;
const DOMAIN: &[u8] = b"BURST/governance-content/v1\0";
/// Signed origin discriminator. The link commits the content hash and the
/// transaction field carries a canonical big-endian length, padded with zeros.
pub const FULL_CONTENT_MARKER: [u8; 32] = *b"BURST_GOV_CONTENT_V1____________";

#[derive(Debug, thiserror::Error)]
pub enum ContentError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("invalid proposal content encoding: {0}")]
    Encoding(String),
}

impl ProposalContent {
    pub fn validate_content(&self) -> Result<(), ContentError> {
        match self {
            Self::ParameterChange { param, new_value }
            | Self::Emergency {
                param, new_value, ..
            } if *new_value > param.maximum_value() => {
                return Err(ContentError::Invalid(
                    "parameter value exceeds storage width",
                ));
            }
            _ => {}
        }
        if let Self::ConstitutionalAmendment { text, .. } = self {
            if text.chars().take(MAX_AMENDMENT_CHARS + 1).count() > MAX_AMENDMENT_CHARS {
                return Err(ContentError::Invalid("amendment exceeds 50000 characters"));
            }
        }
        if let Self::ConstitutionalDiff { title, operations } = self {
            use crate::proposal::AmendmentOp;
            if title.is_empty() || operations.is_empty() {
                return Err(ContentError::Invalid(
                    "amendment needs a title and operations",
                ));
            }
            let mut characters = 0usize;
            for operation in operations {
                let text = match operation {
                    AmendmentOp::AddArticle { title, text } => {
                        if title.is_empty() || text.is_empty() {
                            return Err(ContentError::Invalid(
                                "article title and text must not be empty",
                            ));
                        }
                        text.as_str()
                    }
                    AmendmentOp::ModifyArticle {
                        article_number,
                        new_text,
                    } => {
                        if *article_number == 0 || new_text.is_empty() {
                            return Err(ContentError::Invalid(
                                "article number and new text must be nonempty",
                            ));
                        }
                        new_text.as_str()
                    }
                    AmendmentOp::RepealArticle { article_number } => {
                        if *article_number == 0 {
                            return Err(ContentError::Invalid("article number must be nonzero"));
                        }
                        ""
                    }
                };
                characters += text.chars().take(MAX_AMENDMENT_CHARS + 1).count();
                if characters > MAX_AMENDMENT_CHARS {
                    return Err(ContentError::Invalid("amendment exceeds 50000 characters"));
                }
            }
        }
        let size = bincode::serialized_size(self)
            .map_err(|error| ContentError::Encoding(error.to_string()))?;
        if size > MAX_CONTENT_BYTES as u64 {
            return Err(ContentError::Invalid(
                "proposal content exceeds transfer limit",
            ));
        }
        Ok(())
    }

    pub fn encode_content(&self) -> Result<Vec<u8>, ContentError> {
        self.validate_content()?;
        bincode::serialize(self).map_err(|error| ContentError::Encoding(error.to_string()))
    }

    pub fn decode_content(bytes: &[u8]) -> Result<Self, ContentError> {
        if bytes.is_empty() || bytes.len() > MAX_CONTENT_BYTES {
            return Err(ContentError::Invalid("invalid proposal content length"));
        }
        let content: Self = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(bytes.len() as u64)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map_err(|error| ContentError::Encoding(error.to_string()))?;
        content.validate_content()?;
        Ok(content)
    }

    /// Existing compact parameter proposals use zero-padded 32-byte links.
    /// Content that does not fit must use the full-content transport, never a
    /// truncated prefix or a digest interpreted as proposal bytes.
    pub fn encode_compact(&self) -> Result<[u8; 32], ContentError> {
        let bytes = self.encode_content()?;
        if bytes.len() > 32 {
            return Err(ContentError::Invalid(
                "proposal requires full-content transport",
            ));
        }
        let mut encoded = [0; 32];
        encoded[..bytes.len()].copy_from_slice(&bytes);
        Ok(encoded)
    }

    pub fn decode_compact(bytes: &[u8; 32]) -> Result<Self, ContentError> {
        let content: Self = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(32)
            .allow_trailing_bytes()
            .deserialize(bytes)
            .map_err(|error| ContentError::Encoding(error.to_string()))?;
        if content.encode_compact()? != *bytes {
            return Err(ContentError::Invalid(
                "noncanonical compact proposal padding",
            ));
        }
        Ok(content)
    }
}

fn content_hash(bytes: &[u8]) -> BlockHash {
    let mut input = Vec::with_capacity(DOMAIN.len() + 8 + bytes.len());
    input.extend_from_slice(DOMAIN);
    input.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    input.extend_from_slice(bytes);
    BlockHash::new(burst_crypto::blake2b_256(&input))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentManifest {
    pub hash: BlockHash,
    pub length: u32,
}

impl ContentManifest {
    pub fn block_fields(&self) -> Result<(BlockHash, TxHash, TxHash), ContentError> {
        self.validate()?;
        let mut length = [0; 32];
        length[..4].copy_from_slice(&self.length.to_be_bytes());
        Ok((
            self.hash,
            TxHash::new(FULL_CONTENT_MARKER),
            TxHash::new(length),
        ))
    }

    /// Only call for GovernanceProposal blocks. Other origin domains are not
    /// full-content proposals and must be decoded by their own rules.
    pub fn from_block_fields(
        link: BlockHash,
        origin: TxHash,
        transaction: TxHash,
    ) -> Result<Option<Self>, ContentError> {
        if origin.as_bytes() != &FULL_CONTENT_MARKER {
            return Ok(None);
        }
        if transaction.as_bytes()[4..].iter().any(|byte| *byte != 0) {
            return Err(ContentError::Invalid(
                "noncanonical proposal content length",
            ));
        }
        let manifest = Self {
            hash: link,
            length: u32::from_be_bytes(transaction.as_bytes()[..4].try_into().expect("four bytes")),
        };
        manifest.validate()?;
        Ok(Some(manifest))
    }

    pub fn decode_verified(&self, bytes: &[u8]) -> Result<ProposalContent, ContentError> {
        self.validate()?;
        if bytes.len() != self.length as usize || content_hash(bytes) != self.hash {
            return Err(ContentError::Invalid(
                "proposal content commitment mismatch",
            ));
        }
        ProposalContent::decode_content(bytes)
    }

    pub fn validate(&self) -> Result<(), ContentError> {
        if self.hash.is_zero() || self.length == 0 || self.length as usize > MAX_CONTENT_BYTES {
            return Err(ContentError::Invalid("invalid proposal content manifest"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContentChunk {
    pub manifest: ContentManifest,
    pub offset: u32,
    pub bytes: Vec<u8>,
}

impl ContentChunk {
    fn validate(&self) -> Result<(), ContentError> {
        self.manifest.validate()?;
        let offset = self.offset as usize;
        let length = self.manifest.length as usize;
        if offset >= length
            || !offset.is_multiple_of(CONTENT_CHUNK_BYTES)
            || self.bytes.len() != CONTENT_CHUNK_BYTES.min(length - offset)
        {
            return Err(ContentError::Invalid("invalid proposal content chunk"));
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>, ContentError> {
        self.validate()?;
        bincode::serialize(self).map_err(|error| ContentError::Encoding(error.to_string()))
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ContentError> {
        if bytes.len() > CONTENT_CHUNK_BYTES + 128 {
            return Err(ContentError::Invalid("oversized proposal content chunk"));
        }
        let chunk: Self = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(bytes.len() as u64)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map_err(|error| ContentError::Encoding(error.to_string()))?;
        chunk.validate()?;
        Ok(chunk)
    }
}

pub struct EncodedContent {
    manifest: ContentManifest,
    bytes: Vec<u8>,
}

impl EncodedContent {
    pub fn new(content: &ProposalContent) -> Result<Self, ContentError> {
        let bytes = content.encode_content()?;
        let manifest = ContentManifest {
            hash: content_hash(&bytes),
            length: bytes.len() as u32,
        };
        Ok(Self { manifest, bytes })
    }

    pub fn manifest(&self) -> ContentManifest {
        self.manifest
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn chunks(&self) -> impl Iterator<Item = ContentChunk> + '_ {
        self.bytes
            .chunks(CONTENT_CHUNK_BYTES)
            .enumerate()
            .map(|(index, bytes)| ContentChunk {
                manifest: self.manifest,
                offset: (index * CONTENT_CHUNK_BYTES) as u32,
                bytes: bytes.to_vec(),
            })
    }
}

/// One bounded transfer. The owner must separately bound the number and age of
/// concurrent receivers. Invalid data poisons this receiver; retry with a new
/// receiver and a separately authenticated manifest, not peer-supplied metadata.
pub struct ContentReceiver {
    manifest: ContentManifest,
    chunks: Vec<Option<Vec<u8>>>,
    remaining: usize,
    failed: bool,
}

impl ContentReceiver {
    pub fn new(manifest: ContentManifest) -> Result<Self, ContentError> {
        manifest.validate()?;
        let count = (manifest.length as usize).div_ceil(CONTENT_CHUNK_BYTES);
        Ok(Self {
            manifest,
            chunks: vec![None; count],
            remaining: count,
            failed: false,
        })
    }

    /// Emit validated content once. Identical duplicates and out-of-order
    /// chunks are accepted without growing the receiver's storage.
    pub fn push(&mut self, chunk: &ContentChunk) -> Result<Option<ProposalContent>, ContentError> {
        if self.failed {
            return Err(ContentError::Invalid(
                "proposal content transfer has failed",
            ));
        }
        let result = self.push_inner(chunk);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn push_inner(
        &mut self,
        chunk: &ContentChunk,
    ) -> Result<Option<ProposalContent>, ContentError> {
        chunk.validate()?;
        let length = self.manifest.length as usize;
        if chunk.manifest != self.manifest {
            return Err(ContentError::Invalid("proposal content manifest mismatch"));
        }
        let index = chunk.offset as usize / CONTENT_CHUNK_BYTES;
        if let Some(existing) = &self.chunks[index] {
            if existing != &chunk.bytes {
                return Err(ContentError::Invalid("conflicting proposal content chunk"));
            }
            return Ok(None);
        }
        self.chunks[index] = Some(chunk.bytes.clone());
        self.remaining -= 1;
        if self.remaining != 0 {
            return Ok(None);
        }
        let mut bytes = Vec::with_capacity(length);
        for part in self.chunks.iter().flatten() {
            bytes.extend_from_slice(part);
        }
        let content = self.manifest.decode_verified(&bytes)?;
        Ok(Some(content))
    }
}

#[cfg(test)]
mod tests;
