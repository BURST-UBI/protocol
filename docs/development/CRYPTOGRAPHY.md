# Cryptography

`burst-crypto` provides byte-level operations. Protocol callers decide which
fields are signed, how those fields are encoded and which signer has authority.
A valid signature alone does not make a block or delegation valid.

## Signing keys and signatures

`PrivateKey` stores a 32-byte Ed25519 seed. It is the input to ed25519-dalek's
`SigningKey`, not an expanded secret scalar. `PublicKey` stores 32 bytes and
`Signature` stores 64 bytes.

`sign_message` signs exactly the supplied bytes. `verify_signature` uses
ed25519-dalek's strict verification and rejects malformed or weak keys when
verification fails. Neither helper adds a protocol domain prefix. An operation's
owner must supply its canonical signing bytes and check the signer's authority.

`PrivateKey` zeroizes its owned bytes on drop and has no `Debug`, `Clone` or
serialization implementation. Its byte field is public, so callers can still
copy or expose those bytes; the type does not protect copies made elsewhere.

## Hashes and field boundaries

The hashing helpers use Blake2b with a 32-byte output. This is a configured
Blake2b-256 digest, not a truncation of a 64-byte Blake2b digest.

`blake2b_256_multi` and `blake2b_256_iter` hash the concatenation of their inputs.
They do not record slice boundaries: `["ab", "c"]` and `["a", "bc"]` hash the same
bytes. Protocol encodings must therefore supply field lengths, fixed-width fields
or other unambiguous framing where needed.

`hash_block` and `hash_transaction` only wrap a digest in its corresponding type.
They do not serialize a protocol object, validate it or select the fields covered
by its identity. Use the owning protocol type's hashing method when constructing
an actual operation.

## Wallet addresses

An address has 65 ASCII characters:

| Part | Encoding |
| --- | --- |
| Prefix | `brst_` |
| Public key | 52 base32 characters encoding the 32 key bytes |
| Checksum | 8 base32 characters encoding the first 5 bytes of Blake2b-256 of the key |

The alphabet is `13456789abcdefghijkmnopqrstuwxyz`. Encoding reads bits from the
most significant end of each byte. The final public-key character contains one
key bit followed by four zero padding bits. The checksum is encoded in byte order.

The address decoder checks the prefix, exact length, alphabet, zero padding and
checksum. It does not validate the decoded bytes as an Ed25519 curve point or
prove ownership. Signature verification is a separate check. The checksum detects
encoding mistakes; it is not an authentication mechanism.

## Recovery phrases

Generation encodes a random 32-byte wallet seed as 24 BIP39 words. Recovery
validates the phrase and checksum, then decodes its original 32 bytes of entropy.
It does not apply the BIP39 PBKDF2 seed expansion or accept a BIP39 passphrase.
Shorter BIP39 phrases are rejected rather than padded or truncated.

Account keys follow Nano's deterministic construction:
`BLAKE2b-256(wallet_seed || account_index_be32)`. The current wallet uses index 0.
The result is an Ed25519 signing seed. BURST retains its own signature and address
formats; this does not make its accounts interchangeable with Nano accounts.
A keystore password protects the saved key file and does not change derivation.

This replaces the earlier path-string HMAC scheme. The same old phrase now derives
a different account; no automatic legacy fallback is provided. Existing private-key
and encrypted-keystore imports retain their original account keys. A raw signing
seed is also distinct from the wallet seed used by this derivation.
See [wallet recovery](../wallet/WALLETS.md) for state a phrase cannot restore.

## Delegation-key envelopes

Delegation encryption transports a secondary signing seed to a recipient.
The envelope contains:

| Part | Bytes |
| --- | --- |
| Version marker `BDK` followed by byte `01` | 4 |
| Ephemeral X25519 public key | 32 |
| Random XChaCha20-Poly1305 nonce | 24 |
| Encrypted 32-byte seed and authentication tag | 48 |

Encryption generates a fresh ephemeral X25519 key. The shared secret must be
contributory. The Blake2b-256 key derivation includes the domain
`BURST/delegation-key/v1` followed by a zero byte, the shared secret, both public
keys, the context length as a big-endian u64, and the context bytes. Context must
contain between 1 and 4096 bytes and is also authenticated as associated data.

The caller must derive that context from the authenticated grant, including its
network, owner, delegate, scope, generation and secondary public key. Anyone can
encrypt to a public recipient key; encryption does not establish the grant's
owner. The primary-key signature and protocol grant checks establish authority.

`validate_delegation_envelope` checks only version and length. Decryption checks
the recipient, key exchange and authenticated ciphertext/context before returning
the seed. Code that only checks the envelope shape has not authenticated it.

## Implementation

- [Signing and strict verification](https://github.com/BURST-UBI/protocol/blob/main/crypto/src/sign.rs)
- [Key representations](https://github.com/BURST-UBI/protocol/blob/main/types/src/keys.rs)
- [Key generation and conversion](https://github.com/BURST-UBI/protocol/blob/main/crypto/src/keys.rs)
- [Blake2b helpers](https://github.com/BURST-UBI/protocol/blob/main/crypto/src/hash.rs)
- [Address codec](https://github.com/BURST-UBI/protocol/blob/main/crypto/src/address.rs)
- [Mnemonic derivation](https://github.com/BURST-UBI/protocol/blob/main/crypto/src/mnemonic.rs)
- [Delegation encryption](https://github.com/BURST-UBI/protocol/blob/main/crypto/src/encryption.rs)

## Proof of work

Work is a nonce search. The validator hashes the nonce's eight little-endian bytes
followed by the 32-byte work root, using Blake2b with an eight-byte digest. It
interprets that digest as a little-endian integer and accepts values at or above
the configured threshold. It does not count leading zero bits.

A successor uses its previous block hash as the root. An opening block uses the
Blake2b-256 hash of `BURST/open-work/v2\0` followed by its account address string.
Here `\0` denotes one zero byte. This permits advance work generation while
keeping opening work specific to the account. A threshold increase can invalidate
cached work. Every block kind uses the same current threshold.

The `work_generate` RPC expects this root in its `hash` field. Its `multiplier`
response is a ratio of threshold integers, not a measure of expected search cost.
The block format has a version field; there is no separate work-version field.

### Admission and spam

The node rotates between transport origins and prioritizes higher work within
each origin. Local submissions and recovery have separate origins; network
traffic is grouped by observed IP bucket. Higher work does not bypass capacity
or guarantee confirmation order.

There is no daily transaction-count quota. The separate new-wallet spending cap
limits an individual Send or Burn during its configured age window. Empty account
openings and small positive sends remain possible. Work and bounded queues limit
admission cost and retained work; they do not prove that dust attacks or long-term
ledger growth are harmless.

## Algorithm replacement

Signature and hash algorithms are fixed by the implementation. Governance does
not select an arbitrary replacement, and the block version is not an algorithm
migration mechanism. Incompatible peers fail the exact block/wire-version check.

An algorithm failure would require updated software and a recovery design for
existing keys, identifiers and signed history. The current protocol has no
automatic post-quantum migration or general recovery from compromised signatures
or hashes. See [upgrade limits](../introduction/WHITEPAPER.md#cryptography-and-upgrades).
