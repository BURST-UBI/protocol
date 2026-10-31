use super::*;
const MAX_ROW_BYTES: u64 = 4096;

#[test]
fn recovery_beacons_stream_in_order_at_existing_wire_limit() {
    let dir = tempfile::tempdir().unwrap();
    let mut rows = RecoveryBeacons::new(dir.path()).unwrap();
    let mut beacon = RandomOutput {
        value: [7; 32],
        proof: Vec::new(),
        round: 0,
    };
    let overhead = bincode::serialized_size(&beacon).unwrap() as usize;
    beacon.proof = vec![9; MAX_BEACON_BYTES - overhead];
    for round in 0..1024 {
        beacon.round = round;
        rows.push((burst_types::BlockHash::new([3; 32]), beacon.clone()))
            .unwrap();
    }
    assert_eq!(rows.len(), 1024);
    beacon.proof.push(0);
    assert!(rows
        .push((burst_types::BlockHash::new([3; 32]), beacon))
        .is_err());
    let mut count = 0;
    for row in rows.into_cursor().unwrap() {
        let (hash, beacon) = row.unwrap();
        assert_eq!(hash, burst_types::BlockHash::new([3; 32]));
        assert_eq!(beacon.round, count);
        assert_eq!(beacon.value, [7; 32]);
        assert_eq!(beacon.proof, vec![9; MAX_BEACON_BYTES - overhead]);
        count += 1;
    }
    assert_eq!(count, 1024);
}

#[test]
fn observations_stream_beyond_inventory_count_and_filter_without_losing_global_guards() {
    let dir = tempfile::tempdir().unwrap();
    let mut rows = RecordObservations::new(dir.path()).unwrap();
    rows.push(RecordObservation::new(None, "head".into(), Some(b"head")))
        .unwrap();
    for height in 1..=131_073 {
        rows.push(RecordObservation::new(
            Some(height),
            format!("record/{height}"),
            Some(b"body"),
        ))
        .unwrap();
    }
    assert_eq!(rows.len(), 131_074);
    let mut count = 0;
    rows.visit(|row| {
        if count == 0 {
            assert!(row.height.is_none());
        } else {
            assert_eq!(row.height, Some(count));
        }
        count += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(count, rows.len());
    rows.retain_through(17).unwrap();
    assert_eq!(rows.len(), 18);
    rows.visit(|row| {
        assert!(row.height.is_none_or(|height| height <= 17));
        Ok(())
    })
    .unwrap();
}

#[test]
fn observations_reject_changed_truncated_appended_and_oversized_frames() {
    let dir = tempfile::tempdir().unwrap();
    for case in 0..4 {
        let mut rows = RecordObservations::new(dir.path()).unwrap();
        rows.push(RecordObservation::new(
            Some(1),
            "record".into(),
            Some(b"body"),
        ))
        .unwrap();
        {
            let file = rows.file.get_mut().unwrap();
            match case {
                0 => {
                    file.seek(SeekFrom::End(-1)).unwrap();
                    let mut byte = [0];
                    file.read_exact(&mut byte).unwrap();
                    byte[0] ^= 1;
                    file.seek(SeekFrom::End(-1)).unwrap();
                    file.write_all(&byte).unwrap();
                }
                1 => file.set_len(rows.bytes - 1).unwrap(),
                2 => {
                    file.seek(SeekFrom::End(0)).unwrap();
                    file.write_all(&[0]).unwrap();
                }
                _ => {
                    file.seek(SeekFrom::Start(0)).unwrap();
                    file.write_all(&u64::MAX.to_be_bytes()).unwrap();
                }
            }
        }
        assert!(rows.visit(|_| Ok(())).is_err(), "case {case}");
        assert!(
            rows.fork().is_err(),
            "fork accepted damaged observations: {case}"
        );
        assert!(rows.retain_through(0).is_err(), "case {case}");
    }
    let mut rows = RecordObservations::new(dir.path()).unwrap();
    assert!(rows
        .push(RecordObservation::new(
            None,
            "x".repeat(MAX_ROW_BYTES as usize),
            None
        ))
        .is_err());
    assert_eq!(rows.len(), 0);
    rows.visit(|_| panic!("failed append retained a row"))
        .unwrap();
    rows.push(RecordObservation::new(None, "valid".into(), None))
        .unwrap();
    assert!(rows.visit(|_| Err(invalid("visitor failure"))).is_err());
    rows.visit(|_| Ok(())).unwrap();
}
