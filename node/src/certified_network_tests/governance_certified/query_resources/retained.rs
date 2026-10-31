use super::*;

/// Copy stopped database bodies only. The evidence runner verifies the baseline
/// hashes before launch; LMDB lock files belong to the new process.
pub(in super::super) fn copy_retained(destination: &std::path::Path) -> Option<SnapshotInputs> {
    let source = std::path::PathBuf::from(std::env::var_os("BURST_GOVERNANCE_QUERY_RETAINED")?);
    let inputs = read_query_inputs(&source).expect("retained query metadata prerequisite");
    for peer in 0..3 {
        let name = format!("node-{peer}");
        let target = destination.join(&name);
        std::fs::create_dir(&target).unwrap();
        std::fs::copy(source.join(&name).join("data.mdb"), target.join("data.mdb")).unwrap();
    }
    eprintln!(
        "signed_governance_retained_source {} population={} base={}",
        source.display(),
        inputs.population,
        inputs.base
    );
    Some(inputs)
}

fn read_query_inputs(source: &std::path::Path) -> Result<SnapshotInputs, String> {
    let bytes =
        std::fs::read(source.join("governance-query-inputs.json")).map_err(|e| e.to_string())?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let params: ProtocolParams = bincode::deserialize(
        &hex::decode(
            value["params_bincode_hex"]
                .as_str()
                .ok_or("missing saved parameters")?,
        )
        .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let population = value["human_count"]
        .as_u64()
        .ok_or("missing saved population")?;
    if !(3..=100_000).contains(&population)
        || population > u64::from(params.bootstrap_exit_threshold)
        || params
            != crate::verification_test_support::governance_fixture_params(
                params.bootstrap_exit_threshold,
            )
    {
        return Err("saved governance population or parameters are incompatible".into());
    }
    let base = value["base"].as_u64().ok_or("missing saved base clock")?;
    let now = base.checked_add(20).ok_or("saved clock overflow")?;
    // Older preparation manifests did not record phase/clock. Keep their
    // existing setup-resume semantics; the installed epoch is checked at startup.
    if value
        .get("phase")
        .is_some_and(|phase| !matches!(phase.as_str(), Some("preparing" | "prepared")))
        || value
            .get("now")
            .is_some_and(|saved| saved.as_u64() != Some(now))
    {
        return Err(
            "retained query has advanced beyond preparation; refusing clock rollback".into(),
        );
    }
    let epoch: [u8; 32] = hex::decode(value["epoch"].as_str().ok_or("missing saved epoch")?)
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|_| "invalid saved epoch length")?;
    Ok(SnapshotInputs {
        params,
        population: population as u32,
        epoch: BlockHash::new(epoch),
        base,
        now,
    })
}

fn confirmation_wait_heads(
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    addresses: &[WalletAddress],
) -> BTreeSet<BlockHash> {
    assert!(!stores.is_empty());
    let mut wait = BTreeSet::new();
    for address in addresses {
        let accounts: Vec<_> = stores
            .iter()
            .map(|store| store.account_store().get_account(address).unwrap())
            .collect();
        for account in &accounts {
            assert!(account.confirmation_height <= account.block_count);
        }
        if accounts.iter().any(|account| {
            account.confirmation_height < account.block_count || account.head != accounts[0].head
        }) {
            wait.extend(accounts.iter().map(|account| account.head));
        }
    }
    wait
}

pub(super) async fn reconcile(
    clients: &[NodeClient], stores: &[Arc<burst_store_lmdb::LmdbStore>],
    keys: &[KeyPair], params: &ProtocolParams, base: u64,
) -> Vec<StateBlock> {
    if std::env::var_os("BURST_GOVERNANCE_QUERY_RETAINED").is_none() { return Vec::new(); }
    let addresses: Vec<_> = keys.iter().map(|key| burst_crypto::derive_address(&key.public)).collect();
    let heads = confirmation_wait_heads(stores, &addresses);
    eprintln!("signed_governance_retained_wait_heads count={} peers={}", heads.len(), clients.len());
    if let Ok(expected) = std::env::var("BURST_GOVERNANCE_EXPECT_UNSETTLED_HEADS") {
        assert_eq!(heads.len(), expected.parse::<usize>().unwrap());
    }
    // Skip waits only when every peer has the same fully confirmed tip. Keep
    // ordinary catch-up for unsettled or divergent heads and the pinned checks below.
    for head in heads {
        for client in clients {
            client.wait_confirmed(&head.to_string(), Duration::from_secs(25)).await.unwrap();
        }
    }
    let stores = stores.to_vec();
    let public_keys: Vec<_> = keys.iter().map(|key| key.public.clone()).collect();
    let params_hash = params.params_hash();
    // Reconciliation only reads history. Pin each peer's view without competing
    // for its recovery writer or blocking the async runtime on database work.
    let (proposals, supported) = tokio::task::spawn_blocking(move || {
        let mut proposals = Vec::new();
        let mut supported = BTreeSet::new();
        let mut canonical = None;
        for (peer, store) in stores.iter().enumerate() {
            let snapshot = store.tx_begin_read().unwrap();
            let accounts = store.account_store();
            let blocks = store.block_store();
            let mut observed = Vec::new();
            for (index, address) in addresses.iter().enumerate() {
                let account = accounts.get_account_txn(&snapshot, address).unwrap();
                assert_eq!(account.confirmation_height, account.block_count, "retained tail not settled");
                observed.push(account.head);
                for height in 1..=account.block_count {
                    let hash = blocks.block_at_height_txn(&snapshot, address, height).unwrap().unwrap();
                    let block: StateBlock = bincode::deserialize(&blocks.get_block_txn(&snapshot, &hash).unwrap()).unwrap();
                    if !matches!(block.block_type, BlockType::GovernanceProposal | BlockType::GovernanceEndorse) { continue; }
                    assert_eq!(block.hash, hash);
                    assert_eq!(block.compute_hash(), hash);
                    assert_eq!(&block.account, address);
                    assert!(burst_crypto::verify_signature(hash.as_bytes(), &block.signature, &public_keys[index]));
                    assert_eq!(block.timestamp, Timestamp::new(base + 20));
                    assert_eq!(block.params_hash, params_hash);
                    observed.push(hash);
                    if peer == 0 {
                        if block.block_type == BlockType::GovernanceProposal {
                            assert_eq!(index, 0, "unexpected retained proposer");
                            proposals.push(block);
                        } else {
                            assert_eq!(index, 1, "unexpected retained supporter");
                            assert!(supported.insert(block.link), "duplicate retained support");
                        }
                    }
                }
            }
            if let Some(expected) = &canonical { assert_eq!(&observed, expected, "retained peers disagree"); }
            else { canonical = Some(observed); }
        }
        (proposals, supported)
    }).await.unwrap();
    let hashes: BTreeSet<_> = proposals.iter().map(|p| p.hash).collect();
    assert!(supported.is_subset(&hashes), "retained support lacks proposal");
    for proposal in &proposals {
        if !supported.contains(&proposal.hash) {
            support_rate_proposal(clients, &keys[1], params, base + 20, proposal).await;
        }
    }
    eprintln!("signed_governance_retained_reconciled proposals={} supports={} scope=retained_setup",
        proposals.len(), supported.len());
    proposals
}

pub(in super::super) struct SnapshotInputs {
    pub params: ProtocolParams,
    pub population: u32,
    pub epoch: BlockHash,
    pub base: u64,
    pub now: u64,
}

pub(in super::super) fn copy_snapshot(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> SnapshotInputs {
    assert!(
        std::env::var_os("BURST_GOVERNANCE_QUERY_RETAINED").is_none(),
        "choose one retained source"
    );
    let (version, params, epoch, population, now): (u8, ProtocolParams, BlockHash, u32, u64) =
        bincode::deserialize(&std::fs::read(source.join("resource-probe.bin")).unwrap()).unwrap();
    assert_eq!(version, 1);
    assert!((3..=100_000).contains(&population));
    assert_eq!(
        params,
        crate::verification_test_support::governance_fixture_params(
            params.bootstrap_exit_threshold
        ),
        "snapshot must use the signed governance fixture's economic/timing parameters from genesis"
    );
    for peer in 0..3 {
        let target = destination.join(format!("node-{peer}"));
        std::fs::create_dir(&target).unwrap();
        std::fs::copy(source.join("data.mdb"), target.join("data.mdb")).unwrap();
    }
    let store = burst_store_lmdb::LmdbStore::open(
        &destination.join("node-0"),
        32,
        crate::node::DEFAULT_MAP_SIZE,
    )
    .unwrap();
    let base =
        crate::confirmed_replay::snapshot::installed::load_base(&store, NetworkId::Dev, &params)
            .unwrap()
            .expect("authenticated snapshot prerequisite");
    assert_eq!(base.anchor().hash(), epoch);
    assert_eq!(base.anchor().cutoff().checked_add(1), Some(now));
    assert_eq!(base.anchor().roster().total_humans(), u64::from(population));
    eprintln!(
        "signed_governance_snapshot_source path={} population={population} epoch={epoch} now={now}",
        source.display()
    );
    SnapshotInputs {
        params,
        population,
        epoch,
        base: now.checked_sub(20).unwrap(),
        now,
    }
}

#[test]
fn retained_query_metadata_preserves_parameters_and_rejects_clock_rollback() {
    let directory = tempfile::tempdir().unwrap();
    let params = crate::verification_test_support::governance_fixture_params(100_000);
    let mut value = json!({"params_bincode_hex":hex::encode(bincode::serialize(&params).unwrap()),
        "human_count":8195,"base":850,"epoch":BlockHash::new([7;32]).to_string()});
    let write = |value: &serde_json::Value| {
        std::fs::write(
            directory.path().join("governance-query-inputs.json"),
            serde_json::to_vec(value).unwrap(),
        )
        .unwrap()
    };
    write(&value);
    let saved = read_query_inputs(directory.path()).unwrap();
    assert_eq!(saved.params, params);
    assert_eq!((saved.population, saved.base, saved.now), (8195, 850, 870));
    for phase in ["preparing", "prepared"] {
        value["phase"] = json!(phase);
        value["now"] = json!(870);
        write(&value);
        assert!(read_query_inputs(directory.path()).is_ok());
    }
    value["phase"] = json!("counting");
    value["now"] = json!(900);
    write(&value);
    assert!(read_query_inputs(directory.path())
        .err()
        .unwrap()
        .contains("rollback"));
    value["phase"] = json!("prepared");
    write(&value);
    assert!(read_query_inputs(directory.path())
        .err()
        .unwrap()
        .contains("rollback"));
    value["now"] = json!(870);
    value["human_count"] = json!(100001);
    write(&value);
    assert!(read_query_inputs(directory.path())
        .err()
        .unwrap()
        .contains("population"));
}

#[test]
fn retained_waits_preserve_unsettled_and_divergent_peer_heads() {
    let directories: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let stores: Vec<_> = directories.iter().map(|dir|
        Arc::new(burst_store_lmdb::LmdbStore::open(dir.path(), 32, 8 << 20).unwrap())).collect();
    let address = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[1;32]).public);
    let mut account = burst_store::account::AccountInfo::unopened(address.clone());
    let first = BlockHash::new([2;32]); let later = BlockHash::new([3;32]);
    account.head = first; account.block_count = 1;
    for store in &stores { store.account_store().put_account(&account).unwrap(); }
    assert_eq!(confirmation_wait_heads(&stores, &[address.clone()]), BTreeSet::from([first]));
    for store in &stores { store.account_store().advance_confirmation_height(&address, 1).unwrap(); }
    assert!(confirmation_wait_heads(&stores, &[address.clone()]).is_empty());
    account.head = later; account.block_count = 2; account.confirmation_height = 2;
    stores[2].account_store().put_account(&account).unwrap();
    assert_eq!(confirmation_wait_heads(&stores, &[address.clone()]), BTreeSet::from([first, later]));
    account.confirmation_height = 1;
    for store in &stores[..2] { store.account_store().put_account(&account).unwrap(); }
    assert_eq!(confirmation_wait_heads(&stores, &[address.clone()]), BTreeSet::from([later]));
    for store in &stores { store.account_store().advance_confirmation_height(&address, 2).unwrap(); }
    assert!(confirmation_wait_heads(&stores, &[address]).is_empty());
}
