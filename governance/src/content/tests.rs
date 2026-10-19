use super::*;
use crate::proposal::AmendmentOp;
use crate::GovernableParam;

#[test]
fn diff_content_checks_combined_character_limit_and_operation_shapes() {
    let mut operations = vec![
        AmendmentOp::AddArticle {
            title: "Rights".into(),
            text: "🌍".repeat(MAX_AMENDMENT_CHARS - 1),
        },
        AmendmentOp::ModifyArticle {
            article_number: 1,
            new_text: "人".into(),
        },
        AmendmentOp::RepealArticle { article_number: 2 },
    ];
    let content = ProposalContent::ConstitutionalDiff {
        title: "Changes".into(),
        operations: operations.clone(),
    };
    assert_eq!(
        ProposalContent::decode_content(&content.encode_content().unwrap()).unwrap(),
        content
    );
    operations.push(AmendmentOp::ModifyArticle {
        article_number: 3,
        new_text: "x".into(),
    });
    let invalid = [
        operations,
        vec![],
        vec![AmendmentOp::RepealArticle { article_number: 0 }],
        vec![AmendmentOp::ModifyArticle {
            article_number: 1,
            new_text: String::new(),
        }],
        vec![AmendmentOp::AddArticle {
            title: String::new(),
            text: "x".into(),
        }],
    ];
    for operations in invalid {
        let content = ProposalContent::ConstitutionalDiff {
            title: "Invalid".into(),
            operations,
        };
        assert!(content.encode_content().is_err());
        assert!(ProposalContent::decode_content(&bincode::serialize(&content).unwrap()).is_err());
    }
}

fn amendment() -> ProposalContent {
    ProposalContent::ConstitutionalAmendment {
        title: "共同の憲法 🌍".into(),
        text: "🌍".repeat(MAX_AMENDMENT_CHARS),
    }
}

#[test]
fn maximal_unicode_amendment_roundtrips_in_bounded_reordered_chunks_once() {
    let content = amendment();
    let encoded = EncodedContent::new(&content).unwrap();
    assert!(encoded.manifest().length as usize > burst_types::MAX_P2P_MESSAGE_BYTES);
    let mut receiver = ContentReceiver::new(encoded.manifest()).unwrap();
    let chunks: Vec<_> = encoded.chunks().collect();
    assert_eq!(chunks.len(), 7);
    for (index, chunk) in chunks.iter().rev().enumerate() {
        // Leave room for the wire enum and request metadata.
        assert!(
            bincode::serialized_size(chunk).unwrap() + 1024
                < burst_types::MAX_P2P_MESSAGE_BYTES as u64
        );
        let decoded = ContentChunk::decode(&chunk.encode().unwrap()).unwrap();
        let received = receiver.push(&decoded).unwrap();
        assert_eq!(
            received,
            (index == chunks.len() - 1).then(|| content.clone())
        );
        assert_eq!(receiver.push(chunk).unwrap(), None);
    }
}

#[test]
fn all_proposal_kinds_preserve_their_full_content_and_commitments() {
    let address = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[7; 32]).public);
    let contents = [
        ProposalContent::ParameterChange {
            param: GovernableParam::BrnRate,
            new_value: u128::MAX,
        },
        amendment(),
        ProposalContent::Emergency {
            description: "Descripción de la emergencia".repeat(200),
            param: GovernableParam::TrstExpirySecs,
            new_value: 100,
        },
        ProposalContent::RepresentativeEviction {
            target: address,
            evict: true,
            reason: "Evidence, not an omitted audit field.".repeat(200),
        },
    ];
    let mut hashes = std::collections::HashSet::new();
    for content in contents {
        let encoded = EncodedContent::new(&content).unwrap();
        assert!(hashes.insert(encoded.manifest().hash));
        let mut receiver = ContentReceiver::new(encoded.manifest()).unwrap();
        let mut result = None;
        for chunk in encoded.chunks() {
            if let Some(value) = receiver.push(&chunk).unwrap() {
                result = Some(value);
            }
        }
        assert_eq!(result, Some(content.clone()));
        let bytes = content.encode_content().unwrap();
        assert_eq!(ProposalContent::decode_content(&bytes).unwrap(), content);
        let mut trailing = bytes;
        trailing.push(0);
        assert!(ProposalContent::decode_content(&trailing).is_err());
    }
}

#[test]
fn character_and_byte_limits_are_separate_and_checked_before_transfer() {
    let mut content = amendment();
    assert!(content.encode_content().is_ok());
    if let ProposalContent::ConstitutionalAmendment { text, .. } = &mut content {
        text.push('a');
    }
    assert!(content.encode_content().is_err());
    // Decoder enforces the same character bound against an untrusted writer.
    assert!(ProposalContent::decode_content(&bincode::serialize(&content).unwrap()).is_err());
    let oversized = ProposalContent::Emergency {
        description: "a".repeat(MAX_CONTENT_BYTES),
        param: GovernableParam::BrnRate,
        new_value: 1,
    };
    assert!(oversized.encode_content().is_err());
    for length in [0, MAX_CONTENT_BYTES as u32 + 1, u32::MAX] {
        assert!(ContentReceiver::new(ContentManifest {
            hash: BlockHash::new([1; 32]),
            length
        })
        .is_err());
    }
    assert!(ContentReceiver::new(ContentManifest {
        hash: BlockHash::ZERO,
        length: 8
    })
    .is_err());
    assert!(ProposalContent::decode_content(&[]).is_err());
    assert!(ProposalContent::decode_content(&vec![0; MAX_CONTENT_BYTES + 1]).is_err());
    let mut impossible_string = 1u32.to_le_bytes().to_vec();
    impossible_string.extend_from_slice(&u64::MAX.to_le_bytes());
    assert!(ProposalContent::decode_content(&impossible_string).is_err());
}

#[test]
fn chunks_cannot_change_manifest_offsets_lengths_or_existing_data() {
    let encoded = EncodedContent::new(&amendment()).unwrap();
    let original = encoded.chunks().next().unwrap();
    for mutation in 0..6 {
        let mut receiver = ContentReceiver::new(encoded.manifest()).unwrap();
        let mut chunk = original.clone();
        match mutation {
            0 => chunk.manifest.hash = BlockHash::new([9; 32]),
            1 => chunk.manifest.length -= 1,
            2 => chunk.offset = 1,
            3 => chunk.offset = u32::MAX,
            4 => {
                chunk.bytes.pop();
            }
            5 => {
                chunk.bytes.push(0);
            }
            _ => unreachable!(),
        }
        assert!(receiver.push(&chunk).is_err());
        assert!(
            receiver.push(&original).is_err(),
            "invalid transfer must remain failed"
        );
    }
    let mut receiver = ContentReceiver::new(encoded.manifest()).unwrap();
    assert_eq!(receiver.push(&original).unwrap(), None);
    let mut conflicting = original;
    conflicting.bytes[0] ^= 1;
    assert!(receiver.push(&conflicting).is_err());
}

#[test]
fn corrupt_or_truncated_content_never_emits_a_proposal() {
    let encoded = EncodedContent::new(&amendment()).unwrap();
    let mut chunks: Vec<_> = encoded.chunks().collect();
    let mut receiver = ContentReceiver::new(encoded.manifest()).unwrap();
    for chunk in chunks.iter().take(chunks.len() - 1) {
        assert_eq!(receiver.push(chunk).unwrap(), None);
    }
    let last = chunks.last_mut().unwrap();
    last.bytes[0] ^= 1;
    assert!(receiver.push(last).is_err());
    assert!(receiver.push(last).is_err());
}

#[test]
fn compact_parameters_require_canonical_padding_and_support_zero_values() {
    for new_value in [0, 1, u128::MAX] {
        let content = ProposalContent::ParameterChange {
            param: GovernableParam::BrnRate,
            new_value,
        };
        let mut bytes = content.encode_compact().unwrap();
        assert_eq!(ProposalContent::decode_compact(&bytes).unwrap(), content);
        bytes[31] = 1;
        assert!(ProposalContent::decode_compact(&bytes).is_err());
    }
    assert!(amendment().encode_compact().is_err());
    assert!(ProposalContent::decode_compact(&[255; 32]).is_err());
}

#[test]
fn chunk_wire_decoder_rejects_trailing_truncated_and_forged_lengths() {
    let encoded = EncodedContent::new(&amendment()).unwrap();
    let chunk = encoded.chunks().next().unwrap();
    let bytes = chunk.encode().unwrap();
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(ContentChunk::decode(&trailing).is_err());
    assert!(ContentChunk::decode(&bytes[..bytes.len() - 1]).is_err());
    assert!(ContentChunk::decode(&vec![0; CONTENT_CHUNK_BYTES + 129]).is_err());
    let mut prefix = bincode::serialize(&chunk.manifest).unwrap();
    prefix.extend_from_slice(&0u32.to_le_bytes());
    prefix.extend_from_slice(&u64::MAX.to_le_bytes());
    assert!(ContentChunk::decode(&prefix).is_err());
}
