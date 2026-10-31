use super::*;

/// A local promise, not a certificate. The account/root pair identifies the
/// election; only `block` is the promised block body dependency.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PaymentPromise<'a> {
    Final {
        epoch: BlockHash,
        signer: &'a str,
        account: &'a str,
        root: BlockHash,
        block: BlockHash,
    },
    Fence {
        epoch: BlockHash,
        signer: &'a str,
    },
}

pub(in crate::confirmed_replay::snapshot) fn hash(text: &str) -> Result<BlockHash, StoreError> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("noncanonical signer journal hash"));
    }
    let mut bytes = [0; 32];
    hex::decode_to_slice(text, &mut bytes).map_err(invalid)?;
    Ok(BlockHash::new(bytes))
}

pub(in crate::confirmed_replay::snapshot) fn decode<'a>(
    key: &'a str,
    value: &[u8],
) -> Result<PaymentPromise<'a>, StoreError> {
    let (final_vote, suffix) = if let Some(suffix) = key.strip_prefix("consensus_final/v2/") {
        (true, suffix)
    } else if let Some(suffix) = key.strip_prefix("consensus_fence/v2/") {
        (false, suffix)
    } else {
        return Err(invalid("unsupported payment promise namespace"));
    };
    let mut fields = suffix.split('/');
    let epoch = hash(
        fields
            .next()
            .ok_or_else(|| invalid("missing promise epoch"))?,
    )?;
    let signer = fields
        .next()
        .filter(|s| burst_crypto::decode_address(s).is_some())
        .ok_or_else(|| invalid("invalid promise signer"))?;
    if final_vote {
        let account = fields
            .next()
            .filter(|s| burst_crypto::decode_address(s).is_some())
            .ok_or_else(|| invalid("invalid promise account"))?;
        let root = hash(
            fields
                .next()
                .ok_or_else(|| invalid("missing promise root"))?,
        )?;
        if fields.next().is_some() {
            return Err(invalid("invalid durable promise key"));
        }
        let block = BlockHash::new(value.try_into().map_err(invalid)?);
        if block.is_zero() {
            return Err(invalid("zero durable promise"));
        }
        Ok(PaymentPromise::Final {
            epoch,
            signer,
            account,
            root,
            block,
        })
    } else {
        if fields.next().is_some() || value != epoch.as_bytes() {
            return Err(invalid("snapshot cannot reinterpret a local signing fence"));
        }
        Ok(PaymentPromise::Fence { epoch, signer })
    }
}
