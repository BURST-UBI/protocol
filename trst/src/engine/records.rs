//! Economic records shared by snapshot decoding and row-based storage.
//! Record decoding does not authenticate state or authorize its publication.

use super::*;

/// Logical state records. Storage must version its encoding and bind each
/// record to its key; these values alone are not a database or peer proof.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum WalletRecord {
    Portfolio {
        holder: WalletAddress,
        cached_transferable: u128,
        earliest_expiry: Option<Timestamp>,
    },
    Holding {
        holder: WalletAddress,
        position: u64,
        token: Box<TrstToken>,
    },
    Origin {
        owner: WalletAddress,
        origin: TxHash,
    },
    OriginOwner(WalletAddress),
    Invalidation {
        origin: TxHash,
        wallet_revoked: bool,
    },
    Cause {
        origin: TxHash,
        cause: BlockHash,
    },
}

impl WalletRecord {
    pub const STORAGE_PREFIX: &'static str = "trst_state/v1/";

    fn storage_key(&self) -> Result<String, String> {
        fn hex(bytes: &[u8]) -> String {
            const DIGITS: &[u8; 16] = b"0123456789abcdef";
            let mut result = String::with_capacity(bytes.len() * 2);
            for byte in bytes {
                result.push(DIGITS[(byte >> 4) as usize] as char);
                result.push(DIGITS[(byte & 15) as usize] as char);
            }
            result
        }
        let suffix = match self {
            Self::Portfolio { holder, .. } => format!("0/{}", hex(holder.as_str().as_bytes())),
            Self::Holding {
                holder, position, ..
            } => format!("1/{}/{position:016x}", hex(holder.as_str().as_bytes())),
            Self::OriginOwner(owner) => format!("2/{}", hex(owner.as_str().as_bytes())),
            Self::Origin { origin, .. } => format!("3/{}", hex(origin.as_bytes())),
            Self::Invalidation { origin, .. } => format!("4/{}", hex(origin.as_bytes())),
            Self::Cause { origin, cause } => {
                format!("5/{}/{}", hex(origin.as_bytes()), hex(cause.as_bytes()))
            }
        };
        let key = format!("{}{suffix}", Self::STORAGE_PREFIX);
        if key.len() > 511 {
            return Err("TRST record key exceeds LMDB key limit".into());
        }
        Ok(key)
    }

    /// Versioned encoding with a per-row limit, not a total state-size limit.
    pub fn storage_entry(&self) -> Result<(String, Vec<u8>), String> {
        let size = bincode::serialized_size(&(1u8, self)).map_err(|error| error.to_string())?;
        if size > burst_types::MAX_P2P_MESSAGE_BYTES as u64 {
            return Err("TRST record exceeds byte limit".into());
        }
        Ok((
            self.storage_key()?,
            bincode::serialize(&(1u8, self)).map_err(|error| error.to_string())?,
        ))
    }

    /// Bind a row to its primary key before rebuilding economic state.
    pub fn from_storage_entry(key: &str, bytes: &[u8]) -> Result<Self, String> {
        if !key.starts_with(Self::STORAGE_PREFIX)
            || key.len() > 511
            || bytes.len() > burst_types::MAX_P2P_MESSAGE_BYTES
        {
            return Err("invalid TRST record bounds or namespace".into());
        }
        let (version, record): (u8, Self) = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(burst_types::MAX_P2P_MESSAGE_BYTES as u64)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map_err(|error| error.to_string())?;
        if version != 1 || record.storage_key()? != key {
            return Err("TRST record version or key mismatch".into());
        }
        Ok(record)
    }
}

/// Incremental reconstruction from a single storage transaction. No engine is
/// returned until every record and the final cache/dependency checks succeed.
pub struct WalletRecordReader {
    engine: TrstEngine,
    origins: HashSet<TxHash>,
    failed: bool,
}

impl WalletRecordReader {
    pub fn new(expiry_secs: u64) -> Self {
        Self {
            engine: TrstEngine::with_expiry(expiry_secs),
            origins: HashSet::new(),
            failed: false,
        }
    }

    pub fn push(&mut self, record: WalletRecord) -> Result<(), String> {
        if self.failed {
            return Err("TRST record reader already failed".into());
        }
        let result = self.push_inner(record);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn push_inner(&mut self, record: WalletRecord) -> Result<(), String> {
        match record {
            WalletRecord::Portfolio {
                holder,
                cached_transferable,
                earliest_expiry,
            } => {
                if self
                    .engine
                    .wallets
                    .insert(
                        holder,
                        Shared::from(WalletPortfolio {
                            tokens: Vec::new(),
                            cached_transferable,
                            earliest_expiry,
                        }),
                    )
                    .is_some()
                {
                    return Err("duplicate TRST portfolio".into());
                }
            }
            WalletRecord::Holding {
                holder,
                position,
                token,
            } => {
                if token.holder != holder {
                    return Err("TRST holding owner differs from its record".into());
                }
                let portfolio = self
                    .engine
                    .wallets
                    .get_mut(&holder)
                    .ok_or("TRST holding has no portfolio")?;
                if u64::try_from(portfolio.tokens.len()).ok() != Some(position) {
                    return Err("nonsequential TRST holding position".into());
                }
                portfolio.tokens.push(*token);
            }
            WalletRecord::OriginOwner(owner) => {
                if self
                    .engine
                    .wallet_origins
                    .insert(owner, HashSet::new())
                    .is_some()
                {
                    return Err("duplicate TRST origin owner".into());
                }
            }
            WalletRecord::Origin { owner, origin } => {
                let origins = self
                    .engine
                    .wallet_origins
                    .get_mut(&owner)
                    .ok_or("TRST origin has no owner record")?;
                if !self.origins.insert(origin) || !origins.insert(origin) {
                    return Err("duplicate TRST origin ownership".into());
                }
            }
            WalletRecord::Invalidation {
                origin,
                wallet_revoked,
            } => {
                if self
                    .engine
                    .outcome_invalidations
                    .insert(
                        origin,
                        OutcomeInvalidation {
                            causes: HashSet::new(),
                            wallet_revoked,
                        },
                    )
                    .is_some()
                {
                    return Err("duplicate TRST invalidation".into());
                }
            }
            WalletRecord::Cause { origin, cause } => {
                let invalidation = self
                    .engine
                    .outcome_invalidations
                    .get_mut(&origin)
                    .ok_or("TRST cause has no invalidation")?;
                if cause.is_zero() || !invalidation.causes.insert(cause) {
                    return Err("invalid outcome revocation cause".into());
                }
            }
        }
        Ok(())
    }

    pub fn finish(self) -> Result<TrstEngine, String> {
        self.finish_checked(true)
    }

    pub(super) fn finish_checked(mut self, check_expiry: bool) -> Result<TrstEngine, String> {
        if self.failed {
            return Err("cannot publish failed TRST record reconstruction".into());
        }
        for portfolio in self.engine.wallets.values() {
            portfolio.validate_snapshot()?;
            if check_expiry {
                let expected = portfolio
                    .tokens
                    .iter()
                    .filter(|token| token.state == TrstState::Active)
                    .map(|token| token.earliest_expiry(self.engine.expiry_secs))
                    .min();
                if portfolio.earliest_expiry != expected {
                    return Err("TRST holding expiry cache differs from governed period".into());
                }
            }
        }
        for (origin, invalidation) in &self.engine.outcome_invalidations {
            if !self.origins.contains(origin) || invalidation.causes.is_empty() {
                return Err("invalid outcome revocation causes".into());
            }
        }
        self.engine.rebuild_indexes();
        Ok(self.engine)
    }
}

impl TrstEngine {
    /// Count the rows without sorting portfolios or cloning their holdings.
    pub fn wallet_record_count(&self) -> Option<usize> {
        [
            self.wallets.len(),
            self.wallet_origins.len(),
            self.outcome_invalidations.len(),
        ]
        .into_iter()
        .chain(
            self.wallets
                .values()
                .map(|portfolio| portfolio.tokens.len()),
        )
        .chain(self.wallet_origins.values().map(|origins| origins.len()))
        .chain(
            self.outcome_invalidations
                .values()
                .map(|entry| entry.causes.len()),
        )
        .try_fold(0usize, usize::checked_add)
    }

    /// Emit holdings separately from portfolio caches, and causes separately
    /// from their origin. Expired and revoked holdings are retained unchanged.
    /// The callback can persist each row without collecting an encoded snapshot.
    pub fn visit_wallet_records<E>(
        &self,
        mut visit: impl FnMut(WalletRecord) -> Result<(), E>,
    ) -> Result<(), E> {
        let mut wallets: Vec<_> = self.wallets.iter().collect();
        wallets.sort_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));
        for (holder, portfolio) in wallets {
            visit(WalletRecord::Portfolio {
                holder: holder.clone(),
                cached_transferable: portfolio.cached_transferable,
                earliest_expiry: portfolio.earliest_expiry,
            })?;
            for (position, token) in portfolio.tokens.iter().enumerate() {
                visit(WalletRecord::Holding {
                    holder: holder.clone(),
                    position: position as u64,
                    token: Box::new(token.clone()),
                })?;
            }
        }
        let mut owners: Vec<_> = self.wallet_origins.iter().collect();
        owners.sort_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));
        for (owner, origins) in owners {
            visit(WalletRecord::OriginOwner(owner.clone()))?;
            let mut origins: Vec<_> = origins.iter().copied().collect();
            origins.sort_unstable_by_key(|origin| *origin.as_bytes());
            for origin in origins {
                visit(WalletRecord::Origin {
                    owner: owner.clone(),
                    origin,
                })?;
            }
        }
        let mut invalidations: Vec<_> = self.outcome_invalidations.iter().collect();
        invalidations.sort_unstable_by_key(|(origin, _)| *origin.as_bytes());
        for (origin, invalidation) in invalidations {
            visit(WalletRecord::Invalidation {
                origin: *origin,
                wallet_revoked: invalidation.wallet_revoked,
            })?;
            let mut causes: Vec<_> = invalidation.causes.iter().copied().collect();
            causes.sort_unstable();
            for cause in causes {
                visit(WalletRecord::Cause {
                    origin: *origin,
                    cause,
                })?;
            }
        }
        Ok(())
    }

    /// Restore ordered rows from one consistent storage view. The merger graph
    /// is restored separately and `validate_bases` must run before publication.
    pub fn from_wallet_records(
        records: impl IntoIterator<Item = Result<WalletRecord, String>>,
        expiry_secs: u64,
    ) -> Result<Self, String> {
        let mut builder = WalletRecordReader::new(expiry_secs);
        for record in records {
            builder.push(record?)?;
        }
        builder.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(engine: &TrstEngine) -> Vec<WalletRecord> {
        let mut rows = Vec::new();
        engine
            .visit_wallet_records(|row| {
                rows.push(row);
                Ok::<_, String>(())
            })
            .unwrap();
        assert_eq!(engine.wallet_record_count(), Some(rows.len()));
        rows
    }

    fn fixture() -> TrstEngine {
        let mut engine = TrstEngine::with_expiry(100);
        let owner = WalletAddress::new("brst_owner");
        for id in 1..=2 {
            let token = engine
                .mint(
                    TxHash::new([id; 32]),
                    owner.clone(),
                    50,
                    owner.clone(),
                    Timestamp::new(100 + id as u64),
                )
                .unwrap();
            engine.track_token(token);
        }
        engine
            .wallets
            .insert(WalletAddress::new("brst_empty"), Shared::default());
        engine
            .wallet_origins
            .insert(WalletAddress::new("brst_empty_owner"), HashSet::new());
        engine
    }

    fn roundtrip(engine: &TrstEngine) -> TrstEngine {
        let first = rows(engine);
        let mut entries = std::collections::BTreeMap::new();
        for row in &first {
            let (key, bytes) = row.storage_entry().unwrap();
            assert!(entries.insert(key, bytes).is_none());
        }
        let mut restored = TrstEngine::from_wallet_records(
            entries
                .iter()
                .map(|(key, bytes)| WalletRecord::from_storage_entry(key, bytes)),
            engine.expiry_secs,
        )
        .unwrap();
        restored.merger_graph = engine.merger_graph.clone();
        restored.validate_bases().unwrap();
        assert_eq!(restored.save_wallets(), engine.save_wallets());
        assert_eq!(
            bincode::serialize(&first).unwrap(),
            bincode::serialize(&rows(&restored)).unwrap()
        );
        restored
    }

    #[test]
    fn records_preserve_expiry_and_independent_appeal_restoration() {
        assert!(rows(&TrstEngine::with_expiry(100)).is_empty());
        let mut engine = roundtrip(&fixture());
        let owner = WalletAddress::new("brst_owner");
        let origins = [(owner.clone(), TxHash::new([1; 32]))];
        let first = BlockHash::new([10; 32]);
        let second = BlockHash::new([11; 32]);
        engine.invalidate_outcome_origins(first, &origins).unwrap();
        engine.invalidate_outcome_origins(second, &origins).unwrap();
        engine = roundtrip(&engine);
        engine
            .restore_outcome_origins(first, &origins, Timestamp::new(150))
            .unwrap();
        engine = roundtrip(&engine);
        assert_eq!(engine.wallets[&owner].cached_transferable, 50);
        engine
            .restore_outcome_origins(second, &origins, Timestamp::new(150))
            .unwrap();
        engine = roundtrip(&engine);
        assert_eq!(engine.wallets[&owner].cached_transferable, 100);
        engine.flush_all_expired(Timestamp::new(250));
        engine = roundtrip(&engine);
        assert_eq!(engine.wallets[&owner].cached_transferable, 0);
        engine.set_expiry_period(200, Timestamp::new(250));
        engine = roundtrip(&engine);
        assert_eq!(engine.wallets[&owner].cached_transferable, 100);
    }

    #[test]
    fn storage_rows_bind_keys_versions_and_exact_bytes() {
        for record in rows(&fixture()) {
            let (key, bytes) = record.storage_entry().unwrap();
            assert_eq!(
                WalletRecord::from_storage_entry(&key, &bytes)
                    .unwrap()
                    .storage_entry()
                    .unwrap(),
                (key.clone(), bytes.clone())
            );
            let mut wrong_version = bytes.clone();
            wrong_version[0] = 2;
            assert!(WalletRecord::from_storage_entry(&key, &wrong_version).is_err());
            let mut trailing = bytes.clone();
            trailing.push(0);
            assert!(WalletRecord::from_storage_entry(&key, &trailing).is_err());
            assert!(WalletRecord::from_storage_entry(&key, &bytes[..bytes.len() - 1]).is_err());
            assert!(WalletRecord::from_storage_entry(&format!("{key}0"), &bytes).is_err());
            assert!(
                WalletRecord::from_storage_entry(&key.replace("/v1/", "/v2/"), &bytes).is_err()
            );
        }
        let mut reader = WalletRecordReader::new(100);
        let owner = WalletRecord::OriginOwner(WalletAddress::new("brst_empty"));
        reader.push(owner.clone()).unwrap();
        assert!(reader.push(owner).is_err());
        assert!(
            reader.finish().is_err(),
            "an ignored insertion error must poison reconstruction"
        );
    }

    #[test]
    fn records_reject_missing_duplicate_and_reordered_dependencies() {
        let engine = fixture();
        let valid = rows(&engine);
        for case in 0..8 {
            let mut invalid = valid.clone();
            let holding = invalid
                .iter()
                .position(|row| matches!(row, WalletRecord::Holding { .. }))
                .unwrap();
            match case {
                0 => {
                    invalid.insert(0, invalid[0].clone());
                }
                1 => {
                    invalid.remove(holding);
                }
                2 => {
                    invalid.insert(holding, invalid[holding].clone());
                }
                3 => {
                    invalid.swap(holding, holding + 1);
                }
                4 => {
                    invalid.push(WalletRecord::Origin {
                        owner: WalletAddress::new("brst_missing"),
                        origin: TxHash::new([9; 32]),
                    });
                }
                5 => {
                    invalid.push(WalletRecord::Invalidation {
                        origin: TxHash::new([1; 32]),
                        wallet_revoked: false,
                    });
                }
                6 => {
                    invalid.push(WalletRecord::Cause {
                        origin: TxHash::new([1; 32]),
                        cause: BlockHash::new([9; 32]),
                    });
                }
                _ => {
                    if let WalletRecord::Holding { holder, .. } = &mut invalid[holding] {
                        *holder = WalletAddress::new("brst_empty");
                    }
                }
            }
            assert!(
                TrstEngine::from_wallet_records(invalid.into_iter().map(Ok), 100).is_err(),
                "case {case}"
            );
        }
        assert!(TrstEngine::from_wallet_records(valid.into_iter().map(Ok), 101).is_err());
        assert!(
            TrstEngine::from_wallet_records([Err("storage read failure".into())], 100).is_err()
        );
        let mut visits = 0;
        assert!(engine
            .visit_wallet_records(|_| {
                visits += 1;
                Err::<(), _>("storage write failure")
            })
            .is_err());
        assert_eq!(visits, 1);
        assert_eq!(engine.save_wallets(), fixture().save_wallets());
    }
}
