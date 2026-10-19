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
                BlockType::Epoch | BlockType::Endorse | BlockType::VerificationVote | BlockType::Challenge
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
        // Transition replay can retain an intermediate orchestrator snapshot.
        // The final engine is persisted and checked separately, exactly once.
        self.lifecycle_metadata
            .iter()
            .filter(|(key, _)| key.as_str() != crate::endorsement_commit::SNAPSHOT_KEY)
    }

    /// Shared by full-history and compact installation after their ledger
    /// projection. The caller owns the writer and must abort it on any error.
    pub(crate) fn persist_projection(
        &self,
        batch: &mut LmdbWriteTransaction<'_>,
        anchor: &CheckpointAnchor,
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
        batch.clear_checkpoint_lifecycle()?;
        for (key, value) in self.projected_lifecycle_metadata() {
            batch.put_meta(key, value)?;
        }
        batch.put_meta(
            crate::endorsement_commit::SNAPSHOT_KEY,
            &self.verification.snapshot().encode().map_err(error)?,
        )?;
        for ((address, source), pending) in &self.pending {
            batch.put_pending(
                address,
                source.as_bytes(),
                &bincode::serialize(pending).map_err(error)?,
            )?;
        }
        ledger_updater::persist_economics(batch, &self.brn, &self.trst).map_err(error)?;
        batch.put_brn_meta(
            burst_verification::bootstrap::BootstrapPhase::KEY,
            &self.bootstrap_phase.encoding(),
        )?;
        batch.put_brn_meta(
            b"protocol_params",
            &bincode::serialize(&self.params).map_err(error)?,
        )?;
        let governance = self.governance.clone().unwrap_or_else(|| {
            crate::governance_commit::CandidateGovernance::initial(
                self.params.clone(),
                anchor.network(),
                anchor.genesis_hash(),
            )
        });
        governance.persist(batch).map_err(error)?;
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
        for (token, at) in &self.minted_burns {
            batch.put_origin_index(token, token)?;
            batch.put_expiry_index(
                Timestamp::new(at.as_secs().saturating_add(self.params.trst_expiry_secs)),
                token,
            )?;
        }
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
        let accounts: Vec<_> = self.accounts.values().cloned().collect();
        crate::confirmed_authority::ConfirmedAuthority::from_checkpoint(
            anchor.genesis_hash(),
            &accounts,
        )
        .persist(batch)?;
        batch.put_meta(
            INSTALLED_KEY,
            &authority_bytes(anchor, self.cutoff.as_secs())?,
        )?;
        batch.put_meta(CUTOFF_KEY, &self.cutoff.as_secs().to_be_bytes())?;
        Ok(governance)
    }
}
