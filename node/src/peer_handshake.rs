use crate::wire_message::{HandshakeMsg, WireMessage};
use burst_types::{NetworkId, PrivateKey, PublicKey, Signature};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Clone, Copy)]
pub(crate) enum Role {
    Dialer = 1,
    Listener = 2,
}

fn transcript(
    role: Role,
    listener: &HandshakeMsg,
    dialer: &HandshakeMsg,
) -> Result<Vec<u8>, String> {
    let mut bytes = b"BURST/peer-handshake/v1\0".to_vec();
    bytes.push(role as u8);
    for peer in [listener, dialer] {
        let key =
            burst_crypto::decode_address(peer.node_id.as_str()).ok_or("invalid peer identity")?;
        bytes.extend_from_slice(&key);
        bytes.extend_from_slice(&peer.cookie.ok_or("missing peer challenge")?);
        bytes.push(match peer.network_id {
            NetworkId::Live => 1,
            NetworkId::Test => 2,
            NetworkId::Dev => 3,
        });
        bytes.extend_from_slice(peer.params_hash.as_bytes());
        bytes.push(peer.block_version);
        bytes.extend_from_slice(&peer.wire_version.to_be_bytes());
        bytes.extend_from_slice(&peer.peering_port.to_be_bytes());
    }
    Ok(bytes)
}

pub(crate) fn sign(
    role: Role,
    listener: &HandshakeMsg,
    dialer: &HandshakeMsg,
    key: &PrivateKey,
) -> Result<Signature, String> {
    Ok(burst_crypto::sign_message(
        &transcript(role, listener, dialer)?,
        key,
    ))
}

pub(crate) fn verify(
    role: Role,
    listener: &HandshakeMsg,
    dialer: &HandshakeMsg,
    signature: &Signature,
) -> Result<(), String> {
    let peer = match role {
        Role::Dialer => dialer,
        Role::Listener => listener,
    };
    let key = burst_crypto::decode_address(peer.node_id.as_str()).ok_or("invalid peer identity")?;
    if !burst_crypto::verify_signature(
        &transcript(role, listener, dialer)?,
        signature,
        &PublicKey(key),
    ) {
        return Err("invalid mutual handshake proof".into());
    }
    Ok(())
}

pub(crate) fn validate(peer: &HandshakeMsg, local: &HandshakeMsg) -> Result<(), String> {
    if !peer.supports_current_blocks() || peer.network_id != local.network_id {
        return Err("incompatible peer network or version".into());
    }
    if peer.node_id == local.node_id
        || burst_crypto::decode_address(peer.node_id.as_str()).is_none()
    {
        return Err("invalid or self peer identity".into());
    }
    Ok(())
}

pub(crate) async fn read(reader: &mut (impl AsyncRead + Unpin)) -> Result<HandshakeMsg, String> {
    let size = reader.read_u32().await.map_err(|e| e.to_string())? as usize;
    if size == 0 || size > burst_types::MAX_P2P_MESSAGE_BYTES {
        return Err("invalid handshake size".into());
    }
    let mut bytes = vec![0; size];
    reader
        .read_exact(&mut bytes)
        .await
        .map_err(|e| e.to_string())?;
    match WireMessage::decode(&bytes).map_err(|e| e.to_string())? {
        WireMessage::Handshake(peer) => Ok(peer),
        _ => Err("expected handshake".into()),
    }
}

pub(crate) async fn write(
    writer: &mut (impl AsyncWrite + Unpin),
    message: &HandshakeMsg,
) -> Result<(), String> {
    let bytes = WireMessage::Handshake(message.clone())
        .encode()
        .map_err(|e| e.to_string())?;
    writer
        .write_u32(bytes.len() as u32)
        .await
        .map_err(|e| e.to_string())?;
    writer.write_all(&bytes).await.map_err(|e| e.to_string())?;
    writer.flush().await.map_err(|e| e.to_string())
}

pub(crate) async fn outbound(
    reader: &mut (impl AsyncRead + Unpin),
    writer: &mut (impl AsyncWrite + Unpin),
    mut local: HandshakeMsg,
    key: &PrivateKey,
) -> Result<HandshakeMsg, String> {
    let remote = read(reader).await?;
    validate(&remote, &local)?;
    if remote.cookie.is_none() || remote.cookie_signature.is_some() {
        return Err("invalid listener challenge".into());
    }
    local.cookie = Some(burst_protocol::handshake::create_cookie());
    local.cookie_signature = Some(sign(Role::Dialer, &remote, &local, key)?);
    write(writer, &local).await?;
    let proof = read(reader).await?;
    if proof.cookie.is_some()
        || proof.node_id != remote.node_id
        || proof.network_id != remote.network_id
        || proof.params_hash != remote.params_hash
        || proof.peering_port != remote.peering_port
        || proof.block_version != remote.block_version
        || proof.wire_version != remote.wire_version
    {
        return Err("listener changed handshake context".into());
    }
    verify(
        Role::Listener,
        &remote,
        &local,
        &proof.cookie_signature.ok_or("missing listener proof")?,
    )?;
    Ok(remote)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(seed: u8) -> (HandshakeMsg, PrivateKey) {
        let key = burst_crypto::keypair_from_seed(&[seed; 32]);
        (
            HandshakeMsg {
                node_id: burst_crypto::derive_address(&key.public),
                cookie: Some([seed; 32]),
                cookie_signature: None,
                params_hash: burst_types::BlockHash::new([3; 32]),
                network_id: NetworkId::Dev,
                peering_port: 27076,
                block_version: burst_ledger::CURRENT_BLOCK_VERSION,
                wire_version: burst_types::PROTOCOL_VERSION,
            },
            key.private,
        )
    }

    #[test]
    fn mutual_proofs_bind_roles_identities_challenges_and_all_peer_context() {
        let (listener, listener_key) = peer(1);
        let (dialer, dialer_key) = peer(2);
        let dialer_proof = sign(Role::Dialer, &listener, &dialer, &dialer_key).unwrap();
        let listener_proof = sign(Role::Listener, &listener, &dialer, &listener_key).unwrap();
        verify(Role::Dialer, &listener, &dialer, &dialer_proof).unwrap();
        verify(Role::Listener, &listener, &dialer, &listener_proof).unwrap();
        let wrong_role = sign(Role::Listener, &listener, &dialer, &dialer_key).unwrap();
        assert!(verify(Role::Dialer, &listener, &dialer, &wrong_role).is_err());
        assert!(verify(Role::Listener, &listener, &dialer, &dialer_proof).is_err());
        assert!(verify(Role::Dialer, &listener, &dialer, &listener_proof).is_err());
        for side in 0..2 {
            for field in 0..8 {
                let mut changed_listener = listener.clone();
                let mut changed_dialer = dialer.clone();
                let changed = if side == 0 {
                    &mut changed_listener
                } else {
                    &mut changed_dialer
                };
                match field {
                    0 => changed.node_id = peer(9).0.node_id,
                    1 => changed.cookie = Some([9; 32]),
                    2 => changed.network_id = NetworkId::Live,
                    3 => changed.params_hash = burst_types::BlockHash::new([9; 32]),
                    4 => changed.block_version += 1,
                    5 => changed.wire_version += 1,
                    6 => changed.peering_port += 1,
                    _ => changed.cookie = None,
                }
                assert!(verify(
                    Role::Dialer,
                    &changed_listener,
                    &changed_dialer,
                    &dialer_proof
                )
                .is_err());
                assert!(verify(
                    Role::Listener,
                    &changed_listener,
                    &changed_dialer,
                    &listener_proof
                )
                .is_err());
            }
        }
        assert!(validate(&listener, &listener).is_err());
        validate(&listener, &dialer).unwrap();
    }
}
