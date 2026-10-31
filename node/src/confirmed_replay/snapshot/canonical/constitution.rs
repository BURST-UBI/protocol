use super::*;
use burst_consti::{document::ConstiDocument, ConstiEngine};

struct Votes<'a>(
    &'a std::collections::HashMap<
        burst_types::TxHash,
        std::collections::HashMap<burst_types::WalletAddress, burst_consti::engine::ConstiVote>,
    >,
);
impl Field for Votes<'_> {
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        self.encode_field_at(output, None, 1)
    }
    fn encode_field_at(
        &self,
        output: &mut dyn Write,
        directory: Option<&std::path::Path>,
        depth: usize,
    ) -> Result<(), ReplayError> {
        encode_map(self.0, output, directory, depth, |votes, output, child| {
            Map(votes).encode_field_at(output, directory, child)
        })
    }
}
struct Document<'a>(&'a ConstiDocument);
impl Field for Document<'_> {
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        self.encode_field_at(output, None, 1)
    }
    fn encode_field_at(
        &self,
        mut output: &mut dyn Write,
        directory: Option<&std::path::Path>,
        depth: usize,
    ) -> Result<(), ReplayError> {
        let view = self.0.snapshot_view();
        write_record_at_depth(
            &[
                ("articles", &Sequence(view.articles)),
                ("version", &view.version),
                ("version_history", &Sequence(view.version_history)),
                ("article_index", &Map(view.article_index)),
                ("active_count", &view.active_count),
                ("next_number", &view.next_number),
            ],
            &mut output,
            directory,
            depth,
        )
    }
}

pub(in crate::confirmed_replay::snapshot) struct Constitution<'a>(pub &'a ConstiEngine);
impl Field for Constitution<'_> {
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        self.encode_field_at(output, None, 1)
    }
    fn encode_field_at(
        &self,
        output: &mut dyn Write,
        directory: Option<&std::path::Path>,
        depth: usize,
    ) -> Result<(), ReplayError> {
        self.encode(output, directory, depth, false)
    }
}
impl Constitution<'_> {
    fn encode(
        &self,
        mut output: &mut dyn Write,
        directory: Option<&std::path::Path>,
        depth: usize,
        preflight: bool,
    ) -> Result<(), ReplayError> {
        check_depth(depth)?;
        write_bounded(self.0, &mut std::io::sink(), MAX_SNAPSHOT_BYTES as usize)?;
        let view = self.0.snapshot_view();
        record_at_depth(
            &[
                ("amendments", &Map(view.amendments)),
                ("votes", &Votes(view.votes)),
                ("document", &Document(view.document)),
                ("conflicts", &Map(view.conflicts)),
            ],
            &mut output,
            directory,
            depth,
            preflight,
        )
    }
}

/// Compare canonical representations, retaining both complete encodings in RAM.
/// Scratch storage supports map sorting; it does not hold the output buffers.
pub(in crate::confirmed_replay::snapshot) fn equivalent_constitution(
    actual: &ConstiEngine,
    expected: &ConstiEngine,
    directory: &std::path::Path,
) -> Result<bool, ReplayError> {
    let mut actual_bytes = Vec::new();
    let mut expected_bytes = Vec::new();
    Constitution(actual).encode(&mut actual_bytes, Some(directory), 0, true)?;
    Constitution(expected).encode(&mut expected_bytes, Some(directory), 0, true)?;
    Ok(actual_bytes == expected_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_consti::{
        amendment::Amendment,
        document::VersionEntry,
        engine::{AmendmentConflict, ConstiVote},
    };
    use burst_types::{Timestamp, TxHash, WalletAddress};
    use std::collections::HashMap;

    #[test]
    fn streamed_constitution_preserves_stored_maps_document_depth_and_scratch() {
        #[derive(Serialize)]
        struct Stored {
            amendments: HashMap<TxHash, Amendment>,
            votes: HashMap<TxHash, HashMap<WalletAddress, ConstiVote>>,
            document: ConstiDocument,
            conflicts: HashMap<TxHash, AmendmentConflict>,
        }
        for shape in 0..3 {
            let mut stored = Stored {
                amendments: HashMap::new(),
                votes: HashMap::new(),
                document: ConstiDocument::bootstrap(),
                conflicts: HashMap::new(),
            };
            let first = TxHash::new([7; 32]);
            let template = Amendment {
                hash: first,
                proposer: WalletAddress::new("brst_author"),
                title: "Retained".into(),
                text: "Exact text".into(),
                phase: burst_governance::GovernancePhase::Conflicted,
                votes_yea: 1,
                votes_nay: 2,
                votes_abstain: 3,
                created_at: Timestamp::new(7),
                operations: vec![],
            };
            stored.amendments.insert(first, template.clone());
            stored.votes.insert(first, HashMap::new());
            stored
                .conflicts
                .insert(first, AmendmentConflict::ArticleNotFound(99));
            stored.document.articles[0].repealed = true;
            stored.document.version = 42;
            stored.document.version_history.push(VersionEntry {
                version: 42,
                description: "Stored history".into(),
            });
            for index in 0..if shape > 0 { 4097u64 } else { 3 } {
                if shape == 2 {
                    let mut hash = [0; 32];
                    hash[..8].copy_from_slice(&index.to_le_bytes());
                    let hash = TxHash::new(hash);
                    let mut amendment = template.clone();
                    amendment.hash = hash;
                    stored.amendments.insert(hash, amendment);
                    stored.votes.insert(hash, HashMap::new());
                    stored
                        .document
                        .articles
                        .push(stored.document.articles[0].clone());
                } else {
                    stored.votes.get_mut(&first).unwrap().insert(
                        WalletAddress::new(format!("brst_voter_{index}")),
                        ConstiVote::Abstain,
                    );
                }
            }
            // Decode stored bytes, preserving even cached fields inconsistent with articles.
            let state: ConstiEngine =
                bincode::deserialize(&bincode::serialize(&stored).unwrap()).unwrap();
            let directory = tempfile::tempdir().unwrap();
            let budget = crate::confirmed_replay::scratch_file::ScratchLimit::acquire(
                directory.path(),
                16 << 20,
            )
            .unwrap();
            let mut bytes = Vec::new();
            Constitution(&state)
                .encode(&mut bytes, Some(directory.path()), 0, true)
                .unwrap();
            assert_eq!(bytes, encode(&stored).unwrap());
            assert!(equivalent_constitution(&state, &state, directory.path()).unwrap());
            assert!(
                !equivalent_constitution(&state, &ConstiEngine::new(), directory.path()).unwrap()
            );
            #[derive(Serialize)]
            struct Outer<'a> {
                constitution: &'a ConstiEngine,
            }
            let mut nested = Vec::new();
            encode_record_in(
                &[("constitution", &Constitution(&state))],
                &mut nested,
                Some(directory.path()),
            )
            .unwrap();
            assert_eq!(
                nested,
                encode(&Outer {
                    constitution: &state
                })
                .unwrap()
            );
            if shape == 0 {
                for depth in 59..=65 {
                    let mut legacy = Vec::new();
                    let old = encode_at_depth(&state, &mut legacy, depth);
                    let mut actual = Vec::new();
                    let new = Constitution(&state).encode(
                        &mut actual,
                        Some(directory.path()),
                        depth,
                        true,
                    );
                    assert_eq!(old.is_ok(), new.is_ok(), "depth {depth}");
                    if old.is_ok() {
                        assert_eq!(legacy, actual);
                    } else {
                        assert!(actual.is_empty());
                    }
                }
            }
            assert_eq!(budget.used_temporary_bytes().unwrap(), 0);
            drop(budget);
            if shape > 0 {
                let tiny = crate::confirmed_replay::scratch_file::ScratchLimit::acquire(
                    directory.path(),
                    1,
                )
                .unwrap();
                let mut rejected = Vec::new();
                assert!(Constitution(&state)
                    .encode(&mut rejected, Some(directory.path()), 0, true)
                    .is_err());
                assert!(rejected.is_empty());
                assert_eq!(tiny.used_temporary_bytes().unwrap(), 0);
            }
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        }
    }
}
