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
        block.brn_balance = 1;
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
        assert!(matches!(
            source.get(&BlockHash::ZERO),
            Err(StoreError::NotFound(_))
        ));
    }
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
            block.brn_balance = 1;
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
