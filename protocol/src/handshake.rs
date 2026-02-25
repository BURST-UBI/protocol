//! Node handshake protocol.
//!
//! Authenticates peers via Ed25519 cookie signing:
//!   1. Initiator sends `HandshakeInit` with a random 32-byte cookie.
//!   2. Responder signs the cookie, replies with `HandshakeResponse`.
//!   3. Initiator verifies the signature to confirm the responder's identity.

use burst_crypto::{sign_message, verify_signature};
use burst_types::{BlockHash, NetworkId, PrivateKey, PublicKey, Signature};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::codec;
use crate::version::{is_compatible, PROTOCOL_VERSION};
use crate::ProtocolError;

/// Information about a peer after successful handshake.
pub struct PeerInfo {
    pub node_id: PublicKey,
    pub protocol_version: u16,
    pub network_id: NetworkId,
    /// Deterministic hash of the peer's current ProtocolParams.
    pub params_hash: BlockHash,
}

/// Sent by the initiator to begin the handshake.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandshakeInit {
    pub node_id: PublicKey,
    pub cookie: [u8; 32],
    pub protocol_version: u16,
    pub network_id: NetworkId,
    /// Deterministic hash of the node's current ProtocolParams.
    #[serde(default)]
    pub params_hash: BlockHash,
}

/// Sent by the responder to complete the handshake.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandshakeResponse {
    pub node_id: PublicKey,
    pub cookie_signature: Signature,
    pub protocol_version: u16,
    pub network_id: NetworkId,
    /// Deterministic hash of the node's current ProtocolParams.
    #[serde(default)]
    pub params_hash: BlockHash,
}

/// Generate a random 32-byte cookie for the handshake challenge.
pub fn create_cookie() -> [u8; 32] {
    let mut cookie = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut cookie);
    cookie
}

/// Write a length-prefixed bincode message to a TCP stream.
async fn write_framed(stream: &mut TcpStream, msg: &impl Serialize) -> Result<(), ProtocolError> {
    let frame = codec::encode(msg)?;
    stream
        .write_all(&frame)
        .await
        .map_err(|e| ProtocolError::Io(e.to_string()))?;
    stream
        .flush()
        .await
        .map_err(|e| ProtocolError::Io(e.to_string()))?;
    Ok(())
}

/// Read a length-prefixed bincode message from a TCP stream.
async fn read_framed<T: serde::de::DeserializeOwned>(
    stream: &mut TcpStream,
) -> Result<T, ProtocolError> {
    // Read 4-byte length prefix.
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .map_err(|e| ProtocolError::Io(e.to_string()))?;

    let body_len = u32::from_be_bytes(len_buf) as usize;
    if body_len > codec::MAX_MESSAGE_SIZE {
        return Err(ProtocolError::MessageTooLarge {
            size: body_len,
            max: codec::MAX_MESSAGE_SIZE,
        });
    }

    // Read the body.
    let mut body = vec![0u8; body_len];
    stream
        .read_exact(&mut body)
        .await
        .map_err(|e| ProtocolError::Io(e.to_string()))?;

    codec::decode(&body)
}

/// Initiate a handshake as the connecting peer.
///
/// Sends a `HandshakeInit` with a random cookie, then reads the responder's
/// `HandshakeResponse` and verifies the cookie signature.
pub async fn initiate_handshake(
    stream: &mut TcpStream,
    our_key: &PrivateKey,
    our_public: &PublicKey,
    network: NetworkId,
    our_params_hash: BlockHash,
) -> Result<PeerInfo, ProtocolError> {
    let cookie = create_cookie();

    let init = HandshakeInit {
        node_id: our_public.clone(),
        cookie,
        protocol_version: PROTOCOL_VERSION,
        network_id: network,
        params_hash: our_params_hash,
    };
    write_framed(stream, &init).await?;

    // Read the response.
    let resp: HandshakeResponse = read_framed(stream).await?;

    // Verify protocol version compatibility.
    if !is_compatible(resp.protocol_version) {
        return Err(ProtocolError::UnsupportedVersion(resp.protocol_version));
    }

    // Verify network ID matches.
    if resp.network_id != network {
        return Err(ProtocolError::HandshakeFailed(format!(
            "network mismatch: expected {:?}, got {:?}",
            network, resp.network_id
        )));
    }

    // Verify the cookie signature using the responder's public key.
    if !verify_signature(&cookie, &resp.cookie_signature, &resp.node_id) {
        return Err(ProtocolError::HandshakeFailed(
            "invalid cookie signature from responder".into(),
        ));
    }

    // Also send back our own signature of the cookie so the responder can verify us.
    let our_signature = sign_message(&cookie, our_key);
    write_framed(stream, &our_signature).await?;

    Ok(PeerInfo {
        node_id: resp.node_id,
        protocol_version: resp.protocol_version,
        network_id: resp.network_id,
        params_hash: resp.params_hash,
    })
}

/// Respond to an incoming handshake as the listening peer.
///
/// Reads the initiator's `HandshakeInit`, signs the cookie, sends back a
/// `HandshakeResponse`, then verifies the initiator's identity via their
/// follow-up signature.
pub async fn respond_handshake(
    stream: &mut TcpStream,
    our_key: &PrivateKey,
    our_public: &PublicKey,
    network: NetworkId,
    our_params_hash: BlockHash,
) -> Result<PeerInfo, ProtocolError> {
    // Read the initiator's handshake.
    let init: HandshakeInit = read_framed(stream).await?;

    // Verify protocol version compatibility.
    if !is_compatible(init.protocol_version) {
        return Err(ProtocolError::UnsupportedVersion(init.protocol_version));
    }

    // Verify network ID matches.
    if init.network_id != network {
        return Err(ProtocolError::HandshakeFailed(format!(
            "network mismatch: expected {:?}, got {:?}",
            network, init.network_id
        )));
    }

    // Sign the cookie with our private key.
    let cookie_signature = sign_message(&init.cookie, our_key);

    let resp = HandshakeResponse {
        node_id: our_public.clone(),
        cookie_signature,
        protocol_version: PROTOCOL_VERSION,
        network_id: network,
        params_hash: our_params_hash,
    };
    write_framed(stream, &resp).await?;

    // Read the initiator's proof signature and verify it.
    let initiator_sig: Signature = read_framed(stream).await?;
    if !verify_signature(&init.cookie, &initiator_sig, &init.node_id) {
        return Err(ProtocolError::HandshakeFailed(
            "invalid cookie signature from initiator".into(),
        ));
    }

    Ok(PeerInfo {
        node_id: init.node_id,
        protocol_version: init.protocol_version,
        network_id: init.network_id,
        params_hash: init.params_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_crypto::generate_keypair;
    use burst_types::PrivateKey;
    use tokio::net::TcpListener;

    #[test]
    fn test_create_cookie_is_random() {
        let c1 = create_cookie();
        let c2 = create_cookie();
        assert_ne!(c1, c2);
    }

    #[test]
    fn test_handshake_messages_roundtrip() {
        let kp = generate_keypair();
        let cookie = create_cookie();

        let init = HandshakeInit {
            node_id: kp.public.clone(),
            cookie,
            protocol_version: PROTOCOL_VERSION,
            network_id: NetworkId::Dev,
            params_hash: BlockHash::ZERO,
        };
        let encoded = codec::encode(&init).unwrap();
        let (decoded, _): (HandshakeInit, _) = codec::decode_framed(&encoded).unwrap();
        assert_eq!(decoded.node_id, init.node_id);
        assert_eq!(decoded.cookie, init.cookie);
        assert_eq!(decoded.protocol_version, init.protocol_version);
    }

    #[tokio::test]
    async fn test_full_handshake() {
        let initiator_kp = generate_keypair();
        let responder_kp = generate_keypair();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let responder_private = PrivateKey(responder_kp.private.0);
        let responder_public = responder_kp.public.clone();
        let responder_handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            respond_handshake(
                &mut stream,
                &responder_private,
                &responder_public,
                NetworkId::Dev,
                BlockHash::ZERO,
            )
            .await
        });

        let mut stream = TcpStream::connect(addr).await.unwrap();
        let initiator_result = initiate_handshake(
            &mut stream,
            &initiator_kp.private,
            &initiator_kp.public,
            NetworkId::Dev,
            BlockHash::ZERO,
        )
        .await
        .unwrap();

        let responder_result = responder_handle.await.unwrap().unwrap();

        // Initiator should see responder's public key.
        assert_eq!(initiator_result.node_id, responder_kp.public);
        assert_eq!(initiator_result.network_id, NetworkId::Dev);

        // Responder should see initiator's public key.
        assert_eq!(responder_result.node_id, initiator_kp.public);
        assert_eq!(responder_result.network_id, NetworkId::Dev);
    }

    #[tokio::test]
    async fn test_handshake_network_mismatch() {
        let initiator_kp = generate_keypair();
        let responder_kp = generate_keypair();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let responder_private = PrivateKey(responder_kp.private.0);
        let responder_public = responder_kp.public.clone();
        let _responder_handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            respond_handshake(
                &mut stream,
                &responder_private,
                &responder_public,
                NetworkId::Live,
                BlockHash::ZERO,
            )
            .await
        });

        let mut stream = TcpStream::connect(addr).await.unwrap();
        let result = initiate_handshake(
            &mut stream,
            &initiator_kp.private,
            &initiator_kp.public,
            NetworkId::Dev,
            BlockHash::ZERO,
        )
        .await;

        // Initiator should detect the mismatch in the response.
        assert!(result.is_err());
    }
}
