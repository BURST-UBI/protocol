use super::*;

impl TrstEngine {
    fn outcome_origins(
        &self,
        cause: BlockHash,
        origins: &[(WalletAddress, TxHash)],
    ) -> Result<Vec<TxHash>, TrstError> {
        if cause.is_zero() || origins.is_empty() {
            return Err(TrstError::Other(
                "outcome invalidation needs a cause and origins".into(),
            ));
        }
        let mut ordered = Vec::with_capacity(origins.len());
        for (owner, origin) in origins {
            if !self
                .wallet_origins
                .get(owner)
                .is_some_and(|owned| owned.contains(origin))
            {
                return Err(TrstError::OriginNotFound(origin.to_string()));
            }
            ordered.push(*origin);
        }
        ordered.sort_unstable_by_key(|origin| *origin.as_bytes());
        if ordered.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(TrstError::Other("duplicate outcome origin".into()));
        }
        Ok(ordered)
    }

    /// Invalidate only the named issuance origins. The caller must authenticate
    /// the overturning outcome and derive these origins from its reward receipt.
    /// This engine method does not confer appeal authority.
    pub fn invalidate_outcome_origins(
        &mut self,
        cause: BlockHash,
        origins: &[(WalletAddress, TxHash)],
    ) -> Result<(Vec<RevocationEvent>, Vec<PendingRevocation>), TrstError> {
        let ordered = self.outcome_origins(cause, origins)?;
        for origin in &ordered {
            let wallet_revoked = self.merger_graph.is_origin_revoked(origin);
            self.outcome_invalidations
                .entry(*origin)
                .or_insert_with(|| OutcomeInvalidation {
                    causes: HashSet::new(),
                    wallet_revoked,
                })
                .causes
                .insert(cause);
        }
        Ok(self.revoke_origins(ordered))
    }

    /// Remove one overturned decision's invalidation. Other decisions and an
    /// independently revoked originator still keep the same funds revoked.
    /// Returned restoration effects exclude origins with any remaining cause.
    pub fn restore_outcome_origins(
        &mut self,
        cause: BlockHash,
        origins: &[(WalletAddress, TxHash)],
        now: Timestamp,
    ) -> Result<Vec<UnRevocationResult>, TrstError> {
        let ordered = self.outcome_origins(cause, origins)?;
        let mut restored = Vec::new();
        for origin in ordered {
            let Some(invalidation) = self.outcome_invalidations.get_mut(&origin) else {
                continue;
            };
            if !invalidation.causes.remove(&cause) || !invalidation.causes.is_empty() {
                continue;
            }
            if !invalidation.wallet_revoked {
                restored.push(origin);
            }
            self.outcome_invalidations.remove(&origin);
        }
        Ok(self.restore_origins(restored, now))
    }
}

#[cfg(test)]
mod tests;
