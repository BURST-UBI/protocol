use super::*;
use crate::confirmed_replay::blocks::AppliedBlocks;
use burst_store::StoreError;
use std::borrow::Cow;

struct Owned(StateBlock);
impl AppliedBlocks for Owned {
    fn get(&self, hash: &BlockHash) -> Result<Option<Cow<'_, StateBlock>>, StoreError> {
        Ok((*hash == self.0.hash).then(|| Cow::Owned(self.0.clone())))
    }
}

struct Failed;
impl AppliedBlocks for Failed {
    fn get(&self, _: &BlockHash) -> Result<Option<Cow<'_, StateBlock>>, StoreError> {
        Err(StoreError::Backend("injected replay read failure".into()))
    }
}

#[test]
fn input_sources_preserve_order_and_reject_missing_or_duplicate_blocks() {
    use crate::confirmed_replay::blocks::source::Source;
    let mut fixture = Fixture::new();
    let timestamp = fixture.blocks[0].timestamp.as_secs();
    fixture.push(0, BlockType::Burn, timestamp + 10, |block| {
        block.brn_spent = 1;
        block.link = destination(30);
    });
    // Scans preserve supplied order; causal ordering belongs to the executor.
    fixture.blocks.reverse();
    let directory = tempfile::tempdir().unwrap();
    let memory = Source::new(&fixture.blocks, None).unwrap();
    let disk = Source::new(&fixture.blocks, Some(directory.path())).unwrap();
    for source in [&memory, &disk] {
        for _ in 0..2 {
            let actual: Vec<_> = source
                .iter()
                .map(|block| bincode::serialize(block.unwrap().as_ref()).unwrap())
                .collect();
            let expected: Vec<_> = fixture
                .blocks
                .iter()
                .map(|block| bincode::serialize(block).unwrap())
                .collect();
            assert_eq!(actual, expected);
            assert_eq!(
                source.hashes().collect::<Result<Vec<_>, _>>().unwrap(),
                fixture
                    .blocks
                    .iter()
                    .map(|block| block.hash)
                    .collect::<Vec<_>>()
            );
        }
        let mut scanned = std::collections::BTreeMap::new();
        source
            .visit_unordered(|block| {
                assert!(scanned
                    .insert(block.hash, bincode::serialize(block).unwrap())
                    .is_none());
                Ok::<_, StoreError>(())
            })
            .unwrap();
        let expected = fixture
            .blocks
            .iter()
            .map(|block| (block.hash, bincode::serialize(block).unwrap()))
            .collect();
        assert_eq!(scanned, expected);
        let mut calls = 0;
        let stopped = source.visit_unordered(|_| {
            calls += 1;
            Err::<(), _>(ReplayError::PendingExpiry)
        });
        assert!(matches!(stopped, Err(ReplayError::PendingExpiry)));
        assert_eq!(calls, 1);
        assert!(matches!(
            source.get(&BlockHash::ZERO),
            Err(StoreError::NotFound(_))
        ));
    }
    let certified = std::collections::HashSet::from([fixture.blocks[1].hash]);
    for source in [&memory, &disk] {
        let mut applied = crate::confirmed_replay::blocks::AppliedHistory::from_certified_input(
            source,
            &certified,
            source.directory(),
        )
        .unwrap();
        assert_eq!(applied.len(), 1);
        assert!(applied.get(&fixture.blocks[0].hash).unwrap().is_none());
        assert!(!applied.contains(&fixture.blocks[0].hash).unwrap());
        assert!(applied.encoded(&fixture.blocks[0].hash).is_err());
        assert_eq!(
            applied.encoded(&fixture.blocks[1].hash).unwrap(),
            bincode::serialize(&fixture.blocks[1]).unwrap()
        );
        assert!(applied.insert(&fixture.blocks[1]).is_err());
        applied.insert(&fixture.blocks[0]).unwrap();
        assert_eq!(applied.len(), 2);
        assert!(applied.contains(&fixture.blocks[0].hash).unwrap());
        assert!(applied.insert(&fixture.blocks[0]).is_err());
    }
    let missing = std::collections::HashSet::from([BlockHash::ZERO]);
    assert!(
        crate::confirmed_replay::blocks::AppliedHistory::from_certified_input(
            &disk,
            &missing,
            disk.directory()
        )
        .is_err()
    );
    let duplicates = vec![fixture.blocks[0].clone(), fixture.blocks[0].clone()];
    assert!(Source::new(&duplicates, None).is_err());
    assert!(Source::new(&duplicates, Some(directory.path())).is_err());
    assert!(Source::new(&fixture.blocks, Some(&directory.path().join("missing"))).is_err());
}

#[test]
fn applied_block_readers_preserve_economics_and_propagate_storage_failures() {
    let mut fixture = Fixture::new();
    let genesis = fixture.blocks[0].clone();
    let initial = fixture.replay(genesis.timestamp.as_secs()).unwrap();
    fixture.push(
        0,
        BlockType::Burn,
        genesis.timestamp.as_secs() + 10,
        |block| {
            block.brn_spent = 1;
            block.link = destination(30);
        },
    );
    let block = fixture.blocks.last().unwrap();
    let borrowed = HashMap::from([(genesis.hash, &genesis)]);
    let owned = Owned(genesis.clone());
    assert_eq!(
        owned.encoded(&genesis.hash).unwrap(),
        borrowed.encoded(&genesis.hash).unwrap()
    );
    assert!(!owned.contains(&BlockHash::ZERO).unwrap());
    assert!(matches!(
        owned.encoded(&BlockHash::ZERO),
        Err(StoreError::NotFound(_))
    ));
    let mut expected = initial.clone();
    expected.apply(block, &borrowed, &fixture.params).unwrap();
    let mut actual = initial.clone();
    actual.apply(block, &owned, &fixture.params).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let mut staged =
        crate::confirmed_replay::blocks::AppliedHistory::new(Some(directory.path())).unwrap();
    staged.insert(&genesis).unwrap();
    let mut disk_state = initial.clone();
    disk_state.apply(block, &staged, &fixture.params).unwrap();
    assert_eq!(
        disk_state.snapshot_bytes().unwrap(),
        expected.snapshot_bytes().unwrap()
    );
    assert_eq!(
        actual.snapshot_bytes().unwrap(),
        expected.snapshot_bytes().unwrap()
    );
    let mut failed = initial.clone();
    assert!(failed
        .apply(block, &Failed, &fixture.params)
        .unwrap_err()
        .to_string()
        .contains("injected replay read failure"));
    assert_eq!(
        failed.snapshot_bytes().unwrap(),
        initial.snapshot_bytes().unwrap()
    );
    assert!(Failed.contains(&genesis.hash).is_err());
    assert!(Failed.encoded(&genesis.hash).is_err());
}
