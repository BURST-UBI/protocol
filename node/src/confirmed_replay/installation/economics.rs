use super::*;
use burst_store_lmdb::LmdbWriteTransaction;

pub(crate) fn validate_retired(
    batch: &LmdbWriteTransaction<'_>,
    epoch: BlockHash,
    retired: &[BlockHash],
) -> Result<(), StoreError> {
    for hash in retired {
        let bytes = batch
            .get_meta(&format!("checkpoint_orphan/v1/{epoch}/{hash}"))?
            .ok_or_else(|| error("retired checkpoint block evidence missing"))?;
        let block: StateBlock = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(bytes.len() as u64)
            .reject_trailing_bytes()
            .deserialize(&bytes)
            .map_err(error)?;
        block.validate_format().map_err(error)?;
        if block.hash != *hash || block.compute_hash() != *hash {
            return Err(error("retired checkpoint block identity is corrupt"));
        }
        if !(crate::payment_transition::supports(&block.block_type)
            || governance::supports(&block.block_type)
            || matches!(
                block.block_type,
                BlockType::Epoch
                    | BlockType::Endorse
                    | BlockType::VerificationVote
                    | BlockType::Challenge
            ))
        {
            return Err(error(
                "checkpoint tail requires unsupported lifecycle reconstruction",
            ));
        }
    }
    Ok(())
}

impl ConfirmedReplay {
    pub(crate) fn projected_lifecycle_metadata(&self) -> impl Iterator<Item = (&String, &Vec<u8>)> {
        // Exclude intermediate engine snapshots; persist_projection writes the
        // final verification state separately.
        self.lifecycle_metadata
            .iter()
            .filter(|(key, _)| !crate::verification_storage::is_state_key(key))
    }

    /// Shared by full-history and compact installation after their ledger
    /// projection. The caller owns the writer and must abort it on any error.
    pub(crate) fn persist_projection(
        &self,
        batch: &mut LmdbWriteTransaction<'_>,
        anchor: &CheckpointAnchor,
    ) -> Result<crate::governance_commit::CandidateGovernance, StoreError> {
        self.persist_projection_for_storage(batch, anchor, false)
    }

    /// Full certified ledger replacement has already retired incompatible local
    /// journals. Select stable holdings in that same atomic installation only.
    pub(crate) fn persist_projection_stable(
        &self,
        batch: &mut LmdbWriteTransaction<'_>,
        anchor: &CheckpointAnchor,
    ) -> Result<crate::governance_commit::CandidateGovernance, StoreError> {
        self.persist_projection_for_storage(batch, anchor, true)
    }

    fn persist_projection_for_storage(
        &self,
        batch: &mut LmdbWriteTransaction<'_>,
        anchor: &CheckpointAnchor,
        stable_holdings: bool,
    ) -> Result<crate::governance_commit::CandidateGovernance, StoreError> {
        if self.payment_epoch != anchor.hash()
            || self.cutoff.as_secs() != anchor.cutoff()
            || self.human_roster().map_err(error)? != *anchor.roster()
            || self.lifecycle_metadata.keys().any(|key| {
                !burst_store_lmdb::write_batch::checkpoint_projection::is_lifecycle_key(key)
            })
            || self
                .minted_burns
                .iter()
                .any(|(hash, at)| hash.is_zero() || *at > self.cutoff)
        {
            return Err(error(
                "economic projection differs from its authenticated checkpoint",
            ));
        }
        #[cfg(test)]
        let started = std::time::Instant::now();
        #[cfg(test)]
        let report = |stage: &str, batch: &LmdbWriteTransaction<'_>| {
            if std::env::var_os("BURST_SNAPSHOT_INSTALL_STAGE_LOG").is_some() {
                eprintln!(
                    "snapshot-economics stage={stage} elapsed_ms={}",
                    started.elapsed().as_millis()
                );
                crate::paged_metadata::report_metadata_pages(batch, stage);
            }
        };
        #[cfg(test)]
        report("lifecycle", batch);
        batch.retain_checkpoint_lifecycle(|key| {
            std::str::from_utf8(key).is_ok_and(|key| {
                crate::verification_storage::is_state_key(key)
                    || self.lifecycle_metadata.contains_key(key)
            })
        })?;
        for (key, value) in self.projected_lifecycle_metadata() {
            batch.put_meta(key, value)?;
        }
        #[cfg(test)]
        report("verification-snapshot", batch);
        batch.publish_normalized_recovered_verification(&self.verification)?;
        #[cfg(test)]
        report("installed-authority", batch);
        crate::payment_authority::storage::write(
            batch,
            &authority_bytes(anchor, self.cutoff.as_secs())?,
        )?;
        #[cfg(test)]
        report("installed-cutoff", batch);
        batch.put_meta(CUTOFF_KEY, &self.cutoff.as_secs().to_be_bytes())?;
        #[cfg(test)]
        report("pending", batch);
        for ((address, source), pending) in &self.pending {
            batch.put_pending(
                address,
                source.as_bytes(),
                &bincode::serialize(pending).map_err(error)?,
            )?;
        }
        #[cfg(test)]
        report("brn", batch);
        ledger_updater::persist_brn(batch, &self.brn).map_err(error)?;
        #[cfg(test)]
        report("trst", batch);
        crate::graph_storage::persist(batch, &self.trst.merger_graph)?;
        if stable_holdings {
            crate::trst_storage::stage_replayed_stable(batch, &self.trst)?;
        } else {
            crate::trst_storage::persist(batch, &self.trst)?;
        }
        batch.put_brn_meta(
            burst_verification::bootstrap::BootstrapPhase::KEY,
            &self.bootstrap_phase.encoding(),
        )?;
        batch.put_brn_meta(
            b"protocol_params",
            &bincode::serialize(&self.params).map_err(error)?,
        )?;
        #[cfg(test)]
        report("governance", batch);
        let governance = self.governance.clone().unwrap_or_else(|| {
            crate::governance_commit::CandidateGovernance::initial(
                self.params.clone(),
                anchor.network(),
                anchor.genesis_hash(),
            )
        });
        governance.persist_checkpoint(batch).map_err(error)?;
        crate::confirmed_governance::ConfirmedGovernance::from_checkpoint(
            anchor.hash(),
            self.params.clone(),
            Some(&governance),
        )?
        .persist(batch)?;
        batch.put_meta(
            burst_store::meta::PAYMENT_VOTE_LIMIT_KEY,
            &vote_limit_bytes(anchor.hash(), self.cutoff.as_secs(), Some(&governance))?,
        )?;
        // Mint identity indexes must survive even after a token is fully sent
        // or merged. Current holdings alone cannot reconstruct this inventory.
        #[cfg(test)]
        report("mint-index", batch);
        for (token, at) in &self.minted_burns {
            batch.put_origin_index(token, token)?;
            batch.put_expiry_index(
                Timestamp::new(at.as_secs().saturating_add(self.params.trst_expiry_secs)),
                token,
            )?;
        }
        #[cfg(test)]
        report("proposal-content", batch);
        for (hash, bytes) in &self.proposal_contents {
            let key = crate::confirmed_history::proposal_archive_key(*hash);
            if batch
                .get_meta(&key)?
                .is_some_and(|existing| existing != *bytes)
            {
                return Err(error(
                    "authenticated proposal body conflicts with local archive",
                ));
            }
            batch.put_meta(&key, bytes)?;
        }
        #[cfg(test)]
        report("confirmed-authority", batch);
        let accounts: Vec<_> = self.accounts.values().cloned().collect();
        crate::confirmed_authority::ConfirmedAuthority::from_checkpoint(
            anchor.genesis_hash(),
            &accounts,
        )
        .persist(batch)?;
        #[cfg(test)]
        report("pending-dependencies", batch);
        batch.prepare_pending_dependencies(&self.trst.merger_graph)?;
        Ok(governance)
    }
}
