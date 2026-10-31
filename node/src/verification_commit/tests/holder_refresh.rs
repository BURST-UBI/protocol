use super::*;
use std::cell::Cell;

struct Counted<L> {
    inner: L,
    reads: Cell<usize>,
    writes: Vec<WalletAddress>,
}

impl<L: Ledger> Ledger for Counted<L> {
    fn get_account(&self, address: &WalletAddress) -> Result<AccountInfo, StoreError> {
        self.reads.set(self.reads.get() + 1);
        self.inner.get_account(address)
    }
    fn put_account(&mut self, account: &AccountInfo) -> Result<(), StoreError> {
        self.writes.push(account.address.clone());
        self.inner.put_account(account)
    }
    fn get_block(&self, hash: &BlockHash) -> Result<Vec<u8>, StoreError> {
        self.inner.get_block(hash)
    }
    fn get_meta(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.inner.get_meta(key)
    }
    fn put_meta(&mut self, key: &str, bytes: &[u8]) -> Result<(), StoreError> {
        self.inner.put_meta(key, bytes)
    }
    fn get_pending(
        &self,
        address: &WalletAddress,
        source: &TxHash,
    ) -> Result<PendingInfo, StoreError> {
        self.inner.get_pending(address, source)
    }
    fn put_pending(
        &mut self,
        address: &WalletAddress,
        source: &TxHash,
        pending: &PendingInfo,
    ) -> Result<(), StoreError> {
        self.inner.put_pending(address, source, pending)
    }
    fn take_pending(
        &mut self,
        address: &WalletAddress,
        source: &TxHash,
    ) -> Result<PendingInfo, StoreError> {
        self.inner.take_pending(address, source)
    }
    fn all_pending(&self) -> Result<Vec<(WalletAddress, TxHash, PendingInfo)>, StoreError> {
        self.inner.all_pending()
    }
    fn pending_for_revocation(
        &mut self,
        origins: &[TxHash],
        graph: &burst_trst::MergerGraph,
    ) -> Result<Vec<(WalletAddress, TxHash, PendingInfo)>, StoreError> {
        self.inner.pending_for_revocation(origins, graph)
    }
    fn pending_for_restoration(
        &mut self,
        graph: &burst_trst::MergerGraph,
    ) -> Result<Vec<(WalletAddress, TxHash, PendingInfo)>, StoreError> {
        self.inner.pending_for_restoration(graph)
    }
}

#[test]
fn repeated_origin_events_refresh_each_holder_once_and_preserve_revocation_totals() {
    let mut fixture = Fixture::new();
    fixture.verify_target();
    fixture.add_account(7);
    let holders = [fixture.target.clone(), address(6), address(7)];
    let mut amounts = [0u128; 3];
    for n in 0..128u64 {
        let index = n as usize % holders.len();
        let mut hash = [93; 32];
        hash[..8].copy_from_slice(&n.to_le_bytes());
        let amount = n as u128 + 1;
        let token = fixture
            .trst
            .mint(
                TxHash::new(hash),
                holders[index].clone(),
                amount,
                fixture.target.clone(),
                test_time(1050),
            )
            .unwrap();
        fixture.trst.track_token(token);
        amounts[index] += amount;
    }
    for (holder, amount) in holders.iter().zip(amounts) {
        let mut account = fixture.store.account_store().get_account(holder).unwrap();
        account.trst_balance = amount;
        fixture.store.account_store().put_account(&account).unwrap();
    }
    let mut batch = Counted {
        inner: fixture.store.tx_begin_write().unwrap(),
        reads: Cell::new(0),
        writes: Vec::new(),
    };
    let total = super::super::appeals::revoke_wallet(
        &mut batch,
        &fixture.target,
        test_time(1100),
        &mut fixture.brn,
        &mut fixture.trst,
    )
    .unwrap();
    assert_eq!(total, amounts.iter().sum::<u128>());
    // Three final counter refreshes, plus the target's identity transition.
    assert_eq!(batch.reads.get(), 4);
    assert_eq!(batch.writes.len(), 4);
    for (holder, amount) in holders.iter().zip(amounts) {
        assert_eq!(
            batch.writes.iter().filter(|entry| *entry == holder).count(),
            if holder == &fixture.target { 2 } else { 1 }
        );
        let account = batch.inner.get_account(holder).unwrap();
        assert_eq!(account.revoked_trst, amount);
        assert_eq!(account.trst_balance, amount);
    }
    batch.reads.set(0);
    batch.writes.clear();
    mark_wallet_verified(
        &mut batch,
        &fixture.target,
        test_time(1200),
        &mut fixture.brn,
        &mut fixture.trst,
    )
    .unwrap();
    assert_eq!(batch.reads.get(), 3);
    assert_eq!(batch.writes.len(), 4);
    for (holder, amount) in holders.iter().zip(amounts) {
        let account = batch.inner.get_account(holder).unwrap();
        assert_eq!(account.revoked_trst, 0);
        assert_eq!(account.trst_balance, amount);
        assert_eq!(
            fixture.trst.transferable_balance(holder, test_time(1200)),
            Some(amount)
        );
    }
    drop(batch);
    for (holder, amount) in holders.iter().zip(amounts) {
        let account = fixture.store.account_store().get_account(holder).unwrap();
        assert_eq!(account.revoked_trst, 0);
        assert_eq!(account.trst_balance, amount);
    }
}

#[test]
fn appeal_history_refreshes_each_holder_once_after_all_reward_changes() {
    use super::super::receipts::{self, ChallengeVerdict, Receipt, Reward, Verdict};
    for restoring in [false, true] {
        let mut fixture = Fixture::new();
        fixture.verify_target();
        let holders = [address(4), address(5), address(6)];
        let mut totals = [0u128; 3];
        let root = BlockHash::new([100; 32]);
        let mut previous = None;
        let mut origins = Vec::new();
        // Replay-derived receipts exercise settlement only, not certification.
        for n in 0..32usize {
            let source = BlockHash::new([100 + n as u8; 32]);
            let holder = &holders[n % holders.len()];
            let amount = n as u128 + 1;
            let origin = receipts::reward_origin(source, holder);
            let token = fixture
                .trst
                .mint(
                    origin,
                    holder.clone(),
                    amount,
                    holder.clone(),
                    test_time(1050),
                )
                .unwrap();
            fixture.trst.track_token(token);
            origins.push((holder.clone(), origin));
            totals[n % holders.len()] += amount;
            let result = if restoring {
                ChallengeVerdict::Rejected
            } else {
                ChallengeVerdict::Upheld
            };
            let verdict = match previous {
                None => Verdict::Challenge {
                    admission: source,
                    reason: burst_types::ChallengeReason::Fraud,
                    result,
                },
                Some(reviewed) => Verdict::Appeal {
                    admission: source,
                    reason: burst_types::ChallengeReason::Fraud,
                    result,
                    reviewed,
                },
            };
            let receipt = Receipt {
                source,
                target: fixture.target.clone(),
                completed_at: test_time(1100 + n as u64),
                previous,
                verdict,
                rewards: vec![Reward {
                    recipient: holder.clone(),
                    origin,
                    amount,
                }],
            };
            fixture
                .store
                .meta_store()
                .put_meta(
                    &receipts::key(source),
                    &bincode::serialize(&receipt).unwrap(),
                )
                .unwrap();
            previous = Some(source);
        }
        if restoring {
            fixture
                .trst
                .invalidate_outcome_origins(root, &origins)
                .unwrap();
        }
        for (holder, amount) in holders.iter().zip(totals) {
            let mut account = fixture.store.account_store().get_account(holder).unwrap();
            account.trst_balance = amount;
            account.revoked_trst = if restoring { amount } else { 0 };
            fixture.store.account_store().put_account(&account).unwrap();
        }
        // The latest review already agrees, so identity/bounty changes cannot
        // hide repeated reward-holder writes in the count below.
        let latest = Receipt {
            source: BlockHash::new([200; 32]),
            target: fixture.target.clone(),
            completed_at: test_time(1200),
            previous,
            verdict: Verdict::Appeal {
                admission: BlockHash::new([201; 32]),
                reason: burst_types::ChallengeReason::Fraud,
                result: ChallengeVerdict::Rejected,
                reviewed: previous.unwrap(),
            },
            rewards: Vec::new(),
        };
        fixture
            .store
            .meta_store()
            .put_meta(
                &receipts::key(latest.source),
                &bincode::serialize(&latest).unwrap(),
            )
            .unwrap();
        let current = Receipt {
            source: BlockHash::new([202; 32]),
            completed_at: test_time(1300),
            previous: Some(latest.source),
            verdict: Verdict::Appeal {
                admission: BlockHash::new([203; 32]),
                reason: burst_types::ChallengeReason::Fraud,
                result: ChallengeVerdict::Rejected,
                reviewed: latest.source,
            },
            ..latest.clone()
        };
        let context = Settlement {
            network: burst_types::NetworkId::Dev,
            hash: current.source,
            target: fixture.target.clone(),
            timestamp: current.completed_at,
            sources: Vec::new(),
            authority: SettlementAuthority::SignedVotes,
        };
        let mut batch = Counted {
            inner: fixture.store.tx_begin_write().unwrap(),
            reads: Cell::new(0),
            writes: Vec::new(),
        };
        let bounty = super::super::appeals::settle_review(
            &mut batch,
            &context,
            &current,
            &burst_verification::AppealContext {
                reviewed_outcome: latest.source,
                previously_legitimate: true,
            },
            &mut fixture.brn,
            &mut fixture.trst,
        )
        .unwrap();
        assert_eq!(bounty, 0);
        assert_eq!(batch.reads.get(), holders.len());
        assert_eq!(batch.writes.len(), holders.len());
        for (holder, amount) in holders.iter().zip(totals) {
            assert_eq!(
                batch
                    .writes
                    .iter()
                    .filter(|written| *written == holder)
                    .count(),
                1
            );
            let account = batch.inner.get_account(holder).unwrap();
            assert_eq!(account.trst_balance, amount);
            assert_eq!(account.revoked_trst, if restoring { 0 } else { amount });
        }
        drop(batch);
        for (holder, amount) in holders.iter().zip(totals) {
            let account = fixture.store.account_store().get_account(holder).unwrap();
            assert_eq!(account.trst_balance, amount);
            assert_eq!(account.revoked_trst, if restoring { amount } else { 0 });
        }
    }
}
