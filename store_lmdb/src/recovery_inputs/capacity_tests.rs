use super::*;

fn limits() -> Limits {
    Limits {
        chunks: 8,
        bytes: 8,
        map_bytes: 1 << 20,
    }
}
fn capacity<T>(result: Result<T, InputWriteError>) {
    assert!(matches!(result, Err(InputWriteError::Capacity)));
}
fn invalid_write<T>(result: Result<T, InputWriteError>) {
    assert!(matches!(result, Err(InputWriteError::Storage(_))));
}

#[test]
fn checked_writes_distinguish_capacity_and_preserve_atomic_batches() {
    let directory = tempfile::tempdir().unwrap();
    let root = RecoveryInputs::open(directory.path(), [1; 32], limits()).unwrap();
    let other = root.select_stream_checked([2; 32]).unwrap();
    root.append_checked(0, b"123456").unwrap();
    capacity(other.append_batch_checked(0, &[b"x".to_vec(), b"yz".to_vec()]));
    assert_eq!(
        other.progress().unwrap(),
        Progress {
            positions: 0,
            bytes: 0,
            complete: false
        }
    );
    assert!(other.get(&other.digest(b"x")).unwrap().is_none());
    invalid_write(other.append_checked(1, b"x"));
    invalid_write(other.append_checked(0, b""));
    invalid_write(other.append_batch_checked(0, &[b"too large".to_vec(), vec![]]));
    other.append_checked(0, b"xy").unwrap();
    capacity(root.append_checked(1, b"z"));
    root.seal_checked(1).unwrap();
    invalid_write(root.append_checked(1, b"z"));
    invalid_write(other.seal_checked(2));
    drop(other);
    drop(root);
    let root = RecoveryInputs::open(directory.path(), [1; 32], limits()).unwrap();
    assert_eq!(root.read_position(0).unwrap().unwrap(), b"123456");
    assert_eq!(
        root.select_stream([2; 32])
            .unwrap()
            .read_position(0)
            .unwrap()
            .unwrap(),
        b"xy"
    );
}

#[test]
fn checked_stream_and_position_limits_remain_global() {
    let directory = tempfile::tempdir().unwrap();
    let limits = Limits {
        chunks: 2,
        bytes: 8,
        ..limits()
    };
    let root = RecoveryInputs::open(directory.path(), [1; 32], limits).unwrap();
    let other = root.select_stream_checked([2; 32]).unwrap();
    capacity(root.select_stream_checked([3; 32]));
    root.append_checked(0, b"x").unwrap();
    other.append_checked(0, b"y").unwrap();
    capacity(root.append_checked(1, b"x"));
    assert_eq!(root.progress().unwrap().positions, 1);
    assert_eq!(other.progress().unwrap().positions, 1);
}

#[test]
fn checked_capacity_never_relabels_detected_corruption_or_stale_owners() {
    let directory = tempfile::tempdir().unwrap();
    let root = RecoveryInputs::open(directory.path(), [1; 32], limits()).unwrap();
    root.append_checked(0, b"12345678").unwrap();
    let mut txn = root.env.write_txn().unwrap();
    root.manifest
        .put(&mut txn, &stream_key(&[1; 32]), b"broken")
        .unwrap();
    txn.commit().unwrap();
    invalid_write(root.append_checked(1, b"x"));
    let mut txn = root.env.write_txn().unwrap();
    put_progress(
        root.manifest,
        &mut txn,
        &[1; 32],
        Progress {
            positions: 1,
            bytes: 8,
            complete: false,
        },
    )
    .unwrap();
    root.manifest
        .put(&mut txn, b"positions", &99u64.to_be_bytes())
        .unwrap();
    txn.commit().unwrap();
    // Counter corruption is distinct from adding beyond a valid limit.
    invalid_write(root.append_checked(1, b"x"));
    let mut txn = root.env.write_txn().unwrap();
    root.manifest
        .put(&mut txn, b"positions", &1u64.to_be_bytes())
        .unwrap();
    txn.commit().unwrap();
    root.begin_stream_reset().unwrap();
    invalid_write(root.append_checked(1, b"x"));
    invalid_write(root.select_stream_checked([2; 32]));
    while !root
        .reset_stream_page(128, MAX_CHUNK_BYTES + 64)
        .unwrap()
        .complete
    {}
    invalid_write(root.append_checked(0, b"x"));
    invalid_write(root.seal_checked(1));
    invalid_write(root.select_stream_checked([2; 32]));
    let fresh = RecoveryInputs::open(directory.path(), [1; 32], limits()).unwrap();
    fresh.append_checked(0, b"x").unwrap();
}

#[test]
fn checked_map_full_aborts_and_other_backend_errors_are_not_capacity() {
    let directory = tempfile::tempdir().unwrap();
    let limits = Limits {
        chunks: 8,
        bytes: 1 << 20,
        map_bytes: 1 << 20,
    };
    let root = RecoveryInputs::open(directory.path(), [1; 32], limits).unwrap();
    root.append_checked(0, &vec![1; 512 << 10]).unwrap();
    // Both payloads fit the logical limit exactly; physical overhead exhausts the map.
    capacity(root.append_checked(1, &vec![2; 512 << 10]));
    assert_eq!(root.progress().unwrap().positions, 1);
    assert!(root
        .get(&root.digest(&vec![2; 512 << 10]))
        .unwrap()
        .is_none());
    assert!(matches!(
        InputWriteError::from(heed::Error::Mdb(heed::MdbError::Corrupted)),
        InputWriteError::Storage(_)
    ));
    assert!(matches!(
        InputWriteError::from(heed::Error::Io(std::io::Error::from(
            std::io::ErrorKind::WriteZero
        ))),
        InputWriteError::Storage(_)
    ));
    drop(root);
    let root = RecoveryInputs::open(directory.path(), [1; 32], limits).unwrap();
    assert_eq!(root.read_position(0).unwrap().unwrap(), vec![1; 512 << 10]);
}

#[test]
fn commit_map_full_is_capacity_and_rolls_back_all_progress() {
    let mut commit_failures = 0;
    for prepared in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        // These three stream keys produce B-tree splits that exercise commit-stage
        // MapFull. A different layout may exhaust pages during put instead.
        let hash = |n: u64| {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&n.to_be_bytes());
            burst_types::BlockHash::new(bytes)
        };
        let encoded =
            bincode::serialize(&(burst_types::NetworkId::Dev, hash(8), hash(9), hash(10))).unwrap();
        let context =
            burst_crypto::blake2b_256_multi(&[b"BURST/durable-history-session/v1\0", &encoded]);
        let limits = Limits {
            chunks: 131_072,
            bytes: 1 << 20,
            map_bytes: 1 << 20,
        };
        let session = RecoveryInputs::open(directory.path(), context, limits).unwrap();
        let inventory = session
            .select_stream(burst_crypto::blake2b_256(
                b"BURST/retained-inventory-raw/v1\0",
            ))
            .unwrap();
        let root = inventory.select_stream([44; 32]).unwrap();
        if prepared {
            // Include the inventory header for 201 physical rows.
            inventory
                .append(0, &bincode::serialize(&(1u16, 2u64, 201u64)).unwrap())
                .unwrap();
        }
        COMMIT_CAPACITY.with(|count| count.set(0));
        let mut position = 0u64;
        for size in [16 << 10, 8] {
            loop {
                let mut bytes = vec![11; size];
                bytes[..8].copy_from_slice(&position.to_be_bytes());
                let before = root.progress().unwrap();
                match root.append_checked(position, &bytes) {
                    Ok(_) => position += 1,
                    Err(InputWriteError::Capacity) => {
                        assert_eq!(root.progress().unwrap(), before);
                        assert!(root.get(&root.digest(&bytes)).unwrap().is_none());
                        break;
                    }
                    Err(error) => panic!("unexpected capacity classification: {error}"),
                }
                assert!(position < 2048);
            }
        }
        commit_failures += COMMIT_CAPACITY.with(|count| count.get());
        let expected = root.progress().unwrap();
        let first = root.read_position(0).unwrap().unwrap();
        let last = root.read_position(position - 1).unwrap().unwrap();
        drop(root);
        drop(inventory);
        drop(session);
        let session = RecoveryInputs::open(directory.path(), context, limits).unwrap();
        let root = session.select_stream([44; 32]).unwrap();
        assert_eq!(root.progress().unwrap(), expected);
        assert_eq!(root.read_position(0).unwrap().unwrap(), first);
        assert_eq!(root.read_position(position - 1).unwrap().unwrap(), last);
    }
    assert!(
        commit_failures > 0,
        "fixture missed commit-stage exhaustion"
    );
}
