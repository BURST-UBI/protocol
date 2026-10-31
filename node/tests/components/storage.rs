use super::*;

#[test]
fn lmdb_block_write_read_roundtrip() {
    let (_dir, env) = temp_env();
    let account = make_address(1);
    let rep = make_address(2);

    let block = make_block(
        BlockType::Open,
        &account,
        BlockHash::ZERO,
        &rep,
        0,
        0,
        BlockHash::ZERO,
        TxHash::ZERO,
        1000,
    );

    let bytes = bincode::serialize(&block).unwrap();
    let block_store = env.block_store();
    block_store
        .put_block_with_account(&block.hash, &bytes, &account)
        .unwrap();

    let read_bytes = block_store.get_block(&block.hash).unwrap();
    let read_block: StateBlock = bincode::deserialize(&read_bytes).unwrap();

    assert_eq!(read_block.hash, block.hash);
    assert_eq!(read_block.account, account);
    assert_eq!(read_block.block_type, BlockType::Open);
    assert_eq!(read_block.brn_spent, 0);
    assert_eq!(read_block.trst_balance, 0);
    assert_eq!(read_block.previous, BlockHash::ZERO);
}

#[test]
fn lmdb_frontier_tracks_head() {
    let (_dir, env) = temp_env();
    let account = make_address(3);
    let rep = make_address(4);

    let open = make_block(
        BlockType::Open,
        &account,
        BlockHash::ZERO,
        &rep,
        0,
        100,
        BlockHash::ZERO,
        TxHash::ZERO,
        1000,
    );

    let mut batch = env.tx_begin_write().unwrap();
    let bytes = bincode::serialize(&open).unwrap();
    batch.put_block(&open.hash, &bytes).unwrap();
    batch.put_frontier(&account, &open.hash).unwrap();
    batch.commit().unwrap();

    let frontier_store = env.frontier_store();
    assert_eq!(frontier_store.get_frontier(&account).unwrap(), open.hash);

    let send = make_block(
        BlockType::Send,
        &account,
        open.hash,
        &rep,
        0,
        50,
        BlockHash::new([0xAA; 32]),
        TxHash::ZERO,
        2000,
    );

    let mut batch = env.tx_begin_write().unwrap();
    let bytes = bincode::serialize(&send).unwrap();
    batch.put_block(&send.hash, &bytes).unwrap();
    batch.put_frontier(&account, &send.hash).unwrap();
    batch.commit().unwrap();

    assert_eq!(frontier_store.get_frontier(&account).unwrap(), send.hash);
}

#[test]
fn pending_entry_create_read_delete_roundtrip() {
    let (_dir, env) = temp_env();
    let sender = make_address(180);
    let receiver = make_address(181);
    let rep = make_address(182);

    let send_block = make_block(
        BlockType::Send,
        &sender,
        BlockHash::new([1u8; 32]),
        &rep,
        0,
        700,
        BlockHash::new(pubkey_bytes(&receiver)),
        TxHash::ZERO,
        2000,
    );

    let mut batch = env.tx_begin_write().unwrap();
    burst_node::create_pending_entry(&mut batch, &send_block, 300, &receiver, Vec::new()).unwrap();
    batch.commit().unwrap();

    let pending_store = env.pending_store();
    let send_hash = TxHash::new(*send_block.hash.as_bytes());
    let pending = pending_store.get_pending(&receiver, &send_hash).unwrap();
    assert_eq!(pending.amount, 300);
    assert_eq!(pending.source, sender);

    let receive_block = make_block(
        BlockType::Receive,
        &receiver,
        BlockHash::ZERO,
        &rep,
        0,
        300,
        send_block.hash,
        TxHash::ZERO,
        3000,
    );

    let mut batch = env.tx_begin_write().unwrap();
    burst_node::delete_pending_entry(&mut batch, &receive_block).unwrap();
    batch.commit().unwrap();

    let result = pending_store.get_pending(&receiver, &send_hash);
    assert!(result.is_err(), "pending should be deleted after receive");
}

#[test]
fn write_batch_rollback_on_drop() {
    let (_dir, env) = temp_env();
    let account = make_address(190);
    let rep = make_address(191);

    let block = make_block(
        BlockType::Open,
        &account,
        BlockHash::ZERO,
        &rep,
        0,
        100,
        BlockHash::ZERO,
        TxHash::ZERO,
        1000,
    );

    {
        let mut batch = env.tx_begin_write().unwrap();
        let bytes = bincode::serialize(&block).unwrap();
        batch.put_block(&block.hash, &bytes).unwrap();
        batch.put_frontier(&account, &block.hash).unwrap();
        // Intentionally drop without commit
    }

    let block_store = env.block_store();
    assert!(
        block_store.get_block(&block.hash).is_err(),
        "dropped batch should not persist"
    );

    let frontier_store = env.frontier_store();
    assert!(
        frontier_store.get_frontier(&account).is_err(),
        "dropped batch should not persist frontier"
    );
}
