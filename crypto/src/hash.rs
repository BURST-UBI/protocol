//! Blake2b hashing for blocks and transactions.

use burst_types::{BlockHash, TxHash};

/// Incremental Blake2b-256 hashing without an aggregate input buffer.
pub struct Blake2b256Hasher(blake2b_simd::State);

impl Default for Blake2b256Hasher {
    fn default() -> Self {
        Self(blake2b_simd::Params::new().hash_length(32).to_state())
    }
}

impl Blake2b256Hasher {
    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    pub fn finalize(self) -> [u8; 32] {
        let mut output = [0; 32];
        output.copy_from_slice(self.0.finalize().as_bytes());
        output
    }
}

/// Compute a 256-bit Blake2b hash of arbitrary data.
pub fn blake2b_256(data: &[u8]) -> [u8; 32] {
    let result = blake2b_simd::Params::new().hash_length(32).hash(data);
    let mut output = [0; 32];
    output.copy_from_slice(result.as_bytes());
    output
}

/// Hash the concatenation of the slices without allocating it.
/// Slice boundaries are not encoded; callers must delimit structured fields.
pub fn blake2b_256_multi(parts: &[&[u8]]) -> [u8; 32] {
    blake2b_256_iter(parts.iter().copied())
}

/// Hash streamed byte slices without collecting their contents or references.
pub fn blake2b_256_iter<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> [u8; 32] {
    let mut hasher = Blake2b256Hasher::default();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize()
}

/// Hash caller-supplied bytes and wrap the digest as a `BlockHash`.
/// This helper does not serialize or validate a block.
pub fn hash_block(block_bytes: &[u8]) -> BlockHash {
    BlockHash::new(blake2b_256(block_bytes))
}

/// Hash caller-supplied bytes and wrap the digest as a `TxHash`.
/// This helper does not serialize or validate a transaction.
pub fn hash_transaction(tx_bytes: &[u8]) -> TxHash {
    TxHash::new(blake2b_256(tx_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference_hash(bytes: &[u8]) -> [u8; 32] {
        use blake2::{digest::consts::U32, Blake2b, Digest};
        Blake2b::<U32>::digest(bytes).into()
    }

    #[test]
    fn blake2b_256_known_answers() {
        for (input, expected) in [
            (
                &b""[..],
                "0e5751c026e543b2e8ab2eb06099daa1d1e5df47778f7787faab45cdf12fe3a8",
            ),
            (
                &b"abc"[..],
                "bddd813c634239723171ef3fee98579b94964e3bb1cb3e427262c8c068d52319",
            ),
        ] {
            let expected: Vec<u8> = (0..64)
                .step_by(2)
                .map(|i| u8::from_str_radix(&expected[i..i + 2], 16).unwrap())
                .collect();
            assert_eq!(blake2b_256(input).as_slice(), expected);
            assert_eq!(blake2b_256_iter([input]).as_slice(), expected);
        }
    }

    #[test]
    fn backend_matches_reference_across_lengths_and_stream_boundaries() {
        let bytes: Vec<u8> = (0..16384)
            .map(|i| ((i * 73 + i / 251) % 256) as u8)
            .collect();
        for length in (0..=1025).chain([4096, 16384]) {
            let input = &bytes[..length];
            let expected = reference_hash(input);
            assert_eq!(blake2b_256(input), expected, "length {length}");
            for chunk in [1, 31, 128, 129, 512] {
                let parts = std::iter::once(&[][..])
                    .chain(input.chunks(chunk))
                    .chain([&[][..]]);
                assert_eq!(
                    blake2b_256_iter(parts),
                    expected,
                    "length {length}, chunk {chunk}"
                );
            }
        }
    }

    #[test]
    fn blake2b_deterministic() {
        let h1 = blake2b_256(b"hello burst");
        let h2 = blake2b_256(b"hello burst");
        assert_eq!(h1, h2);
    }

    #[test]
    fn blake2b_different_inputs() {
        let h1 = blake2b_256(b"hello");
        let h2 = blake2b_256(b"world");
        assert_ne!(h1, h2);
    }

    #[test]
    fn blake2b_empty() {
        let h = blake2b_256(b"");
        assert_ne!(h, [0u8; 32]);
    }

    #[test]
    fn blake2b_multi_equivalent() {
        let single = blake2b_256(b"helloworld");
        let multi = blake2b_256_multi(&[b"hello", b"world"]);
        assert_eq!(single, multi);
    }

    #[test]
    fn streamed_hash_preserves_empty_parts_and_compression_boundaries() {
        let bytes: Vec<u8> = (0..1025).map(|i| (i % 251) as u8).collect();
        for length in [0, 1, 127, 128, 129, 255, 256, 257, 1025] {
            let input = &bytes[..length];
            for chunk in [1, 31, 128, 129, 512] {
                let parts = std::iter::once(&[][..])
                    .chain(input.chunks(chunk))
                    .chain(std::iter::once(&[][..]));
                assert_eq!(blake2b_256_iter(parts), blake2b_256(input));
                let mut incremental = Blake2b256Hasher::default();
                incremental.update(&[]);
                for part in input.chunks(chunk) {
                    incremental.update(part);
                }
                incremental.update(&[]);
                assert_eq!(incremental.finalize(), blake2b_256(input));
            }
        }
        assert_eq!(blake2b_256_iter(std::iter::empty()), blake2b_256(b""));
    }

    #[test]
    fn hash_block_returns_blockhash() {
        let h = hash_block(b"test block data");
        assert!(!h.is_zero());
    }

    #[test]
    fn hash_transaction_returns_txhash() {
        let h = hash_transaction(b"test tx data");
        assert!(!h.is_zero());
    }
}
