//! Diagnostic only: use the production decoder and head commitment for exported records.
use crate::confirmed_history::{head_commitment, HistoryHead};
use crate::checkpoint_store::{chain, MAX_RECORD_BYTES};

#[derive(Debug, Default, PartialEq, Eq)]
struct Changes { unchanged: usize, changed: usize, added: usize, removed: usize }

fn changes(previous: &[HistoryHead], current: &[HistoryHead]) -> Changes {
    let mut counts = Changes::default();
    let (mut old, mut new) = (previous.iter().peekable(), current.iter().peekable());
    while let (Some(a), Some(b)) = (old.peek(), new.peek()) {
        match a.account.cmp(&b.account) {
            std::cmp::Ordering::Less => { counts.removed += 1; old.next(); }
            std::cmp::Ordering::Greater => { counts.added += 1; new.next(); }
            std::cmp::Ordering::Equal => {
                if a == b { counts.unchanged += 1; } else { counts.changed += 1; }
                old.next(); new.next();
            }
        }
    }
    counts.removed += old.count();
    counts.added += new.count();
    counts
}

#[test]
fn merge_counts_additions_removals_and_changed_heads() {
    use burst_types::{BlockHash, WalletAddress};
    let head = |account: &str, height| HistoryHead {
        account: WalletAddress::new(account), height, hash: BlockHash::new([1; 32]),
    };
    let old = vec![head("brst_a", 1), head("brst_b", 1), head("brst_d", 1)];
    let new = vec![head("brst_b", 2), head("brst_c", 1), head("brst_d", 1), head("brst_e", 1)];
    assert_eq!(changes(&old, &new), Changes { unchanged: 1, changed: 1, added: 2, removed: 1 });
    assert_eq!(changes(&[], &new).added, 4);
    assert_eq!(changes(&old, &[]).removed, 3);
    assert_eq!(changes(&old, &old).unchanged, 3);
    let mut replaced = old.clone();
    replaced[0].hash = BlockHash::new([2; 32]);
    assert_eq!(changes(&old, &replaced), Changes { unchanged: 2, changed: 1, added: 0, removed: 0 });
}

/// Account for candidate node payloads using the shared production tree builder.
/// This is not an LMDB implementation or a measurement of allocated disk pages.
#[derive(Default)]
struct TreeFootprint {
    seen: std::collections::HashSet<[u8; 32]>,
    leaves: usize,
    branches: usize,
    payload_bytes: usize,
}

impl TreeFootprint {
    fn append(&mut self, node: crate::ordered_radix::Node<[u8; 40], [u8; 32]>)
        -> Result<[u8; 32], burst_store::StoreError> {
        use crate::ordered_radix::Node;
        let value = match node {
            Node::Leaf { key, value } => burst_store_lmdb::checkpoint_heads::HeadNode::Leaf {
                account: key, height: u64::from_be_bytes(value[..8].try_into().unwrap()),
                hash: value[8..].try_into().unwrap() },
            Node::Branch { bit, left, right } => burst_store_lmdb::checkpoint_heads::HeadNode::Branch { bit, left, right },
        };
        let leaf = matches!(value, burst_store_lmdb::checkpoint_heads::HeadNode::Leaf { .. });
        let encoded = value.encode()?;
        let hash = value.digest()?;
        if self.seen.insert(hash) {
            self.payload_bytes += encoded.len();
            if leaf { self.leaves += 1; } else { self.branches += 1; }
        }
        Ok(hash)
    }

    fn observe(&mut self, heads: &[HistoryHead]) {
        let mut builder = crate::ordered_radix::Builder::new();
        for head in heads {
            let key = burst_crypto::decode_address(head.account.as_str()).unwrap();
            assert_eq!(burst_crypto::derive_address(&burst_types::PublicKey(key)), head.account);
            let mut value = [0; 40];
            value[..8].copy_from_slice(&head.height.to_be_bytes());
            value[8..].copy_from_slice(head.hash.as_bytes());
            builder.push(key, value, |node| self.append(node)).unwrap();
        }
        let (root, count) = builder.finish(|node| self.append(node)).unwrap();
        assert!(root.is_some()); assert_eq!(count, heads.len());
    }
}

pub(super) fn record_paths() -> Vec<(u64, std::path::PathBuf)> {
    let directory = std::path::PathBuf::from(std::env::var_os("BURST_RECORD_REUSE_INPUTS")
        .expect("set directory containing <height>.bin logical records from the immutable fixture"));
    let expected: usize = std::env::var("BURST_RECORD_REUSE_COUNT")
        .expect("set expected record count").parse().unwrap();
    assert!((2..=128).contains(&expected));
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        assert!(entry.file_type().unwrap().is_file());
        let path = entry.path();
        assert_eq!(path.extension().and_then(|s| s.to_str()), Some("bin"));
        let height: u64 = path.file_stem().unwrap().to_str().unwrap().parse().unwrap();
        assert!(height > 0);
        paths.push((height, path));
        assert!(paths.len() <= expected);
    }
    assert_eq!(paths.len(), expected);
    paths.sort_unstable_by_key(|(height, _)| *height);
    assert!(paths.windows(2).all(|pair| pair[0].0 < pair[1].0));
    paths
}

#[test]
#[ignore = "capped-host read-only diagnostic over exported logical checkpoint records"]
fn historical_head_reuse_probe() {
    let paths = record_paths();
    let mut previous = Vec::new();
    let mut identity = None;
    let mut tree = TreeFootprint::default();
    let mut total = 0usize;
    let mut unchanged = 0usize;
    for (height, path) in paths {
        assert!(path.metadata().unwrap().len() <= MAX_RECORD_BYTES);
        let bytes = std::fs::read(path).unwrap();
        let record = chain::decode_record(&bytes).unwrap();
        let checkpoint = &record.certificate().checkpoint;
        assert_eq!(checkpoint.height, height);
        let next_identity = (checkpoint.network, checkpoint.genesis);
        if let Some(identity) = identity { assert_eq!(identity, next_identity); }
        identity = Some(next_identity);
        let heads = record.heads();
        assert_eq!(head_commitment(checkpoint.network, checkpoint.genesis,
            checkpoint.cutoff, heads).unwrap(), checkpoint.ledger_root);
        tree.observe(heads);
        let counts = changes(&previous, heads);
        assert_eq!(counts.unchanged + counts.changed + counts.added, heads.len());
        assert_eq!(counts.unchanged + counts.changed + counts.removed, previous.len());
        println!("HISTORY_HEAD_REUSE height={height} heads={} logical_bytes={} unchanged={} changed={} added={} removed={}",
            heads.len(), bytes.len(), counts.unchanged, counts.changed, counts.added, counts.removed);
        total += heads.len(); unchanged += counts.unchanged;
        previous = heads.to_vec();
    }
    println!("HISTORY_TREE_MODEL unique_leaves={} unique_branches={} payload_bytes={} binary_keys_and_u64_refcounts_bytes={} ascii_meta_keys_and_u64_refcounts_bytes={}",
        tree.leaves, tree.branches, tree.payload_bytes, tree.seen.len() * (32 + 8),
        tree.seen.len() * ("checkpoint_heads/node/v1/".len() + 64 + 8));
    println!("HISTORY_TREE_MODEL_SCOPE excludes_lmdb_pages=true excludes_record_descriptors=true storage_implementation=false");
    println!("HISTORY_HEAD_REUSE_TOTAL occurrences={total} unchanged_from_previous={unchanged}");
    // Decoding and matching a commitment do not authenticate signatures or replay economics.
    println!("HISTORY_HEAD_REUSE_SCOPE signatures=false replay=false acceptance=false");
}
