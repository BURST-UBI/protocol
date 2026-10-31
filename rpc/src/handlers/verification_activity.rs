use super::*;
use burst_types::WalletState;
use burst_verification::state::VerificationPhase;
use std::{cmp::Reverse, collections::BTreeMap};

/// Retain the newest entries by timestamp, breaking ties by encounter order.
struct Recent<T> {
    entries: BTreeMap<(Reverse<u64>, usize), T>,
    seen: usize,
    limit: usize,
}

impl<T> Recent<T> {
    fn new(limit: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            seen: 0,
            limit,
        }
    }
    fn push(&mut self, timestamp: u64, value: impl FnOnce() -> T) {
        let key = (Reverse(timestamp), self.seen);
        self.seen += 1;
        if self.limit == 0
            || (self.entries.len() == self.limit
                && self
                    .entries
                    .last_key_value()
                    .is_some_and(|(last, _)| *last <= key))
        {
            return;
        }
        self.entries.insert(key, value());
        if self.entries.len() > self.limit {
            self.entries.pop_last();
        }
    }
    fn values(self) -> impl Iterator<Item = T> {
        self.entries.into_values()
    }
}

/// Build activity from a full account scan and a separately captured engine view.
/// Keep up to 50 verification timestamps and endorsements, newest first. Accounts
/// with a verification timestamp remain eligible regardless of their current state.
/// Challenges take the first 50 active entries in engine order, not timestamp order.
/// These display counts do not share one atomic storage snapshot.
pub(super) async fn read(state: &RpcState) -> Result<serde_json::Value, RpcError> {
    let account_store = state.account_store.clone();
    let verification_engine = state.verification_engine.clone();
    state.read_executor.run(move || {
        let mut verified = Recent::new(50);
        let (mut total_verified, mut total_revoked, mut verifier_pool_size) = (0usize, 0usize, 0usize);
        account_store.visit_accounts(&mut |account| {
            if account.state == WalletState::Verified {
                total_verified += 1;
                verifier_pool_size += usize::from(account.verifier_opted_in_at.is_some());
            }
            total_revoked += usize::from(account.state == WalletState::Revoked);
            if let Some(at) = account.verified_at {
                verified.push(at.as_secs(), || account);
            }
            Ok(())
        })?;
        let engine = verification_engine.blocking_lock().activity_snapshot();
        let mut endorsements = Recent::new(50);
        let (mut active_verifications, mut pending_endorsements) = (0usize, 0usize);
        for process in engine.states() {
            active_verifications += usize::from(matches!(
                process.phase,
                VerificationPhase::Endorsing
                    | VerificationPhase::Voting
                    | VerificationPhase::Challenged
            ));
            if process.phase == VerificationPhase::Endorsing {
                pending_endorsements += process.endorsements.len();
            }
            for endorsement in &process.endorsements {
                endorsements.push(endorsement.timestamp.as_secs(), || {
                    (&process.target, endorsement)
                });
            }
        }
        let endorsements: Vec<_> = endorsements.values().map(|(target, e)| serde_json::json!({
            "endorser":e.endorser.as_str(), "target":target.as_str(), "amount":e.burn_amount.to_string(), "timestamp":e.timestamp.as_secs()
        })).collect();
        let verified: Vec<_> = verified.values().map(|a| serde_json::json!({
            "account":a.address.as_str(),"result":format!("{:?}",a.state).to_lowercase(),"timestamp":a.verified_at.unwrap().as_secs(),
            "verifier_count":engine.get_state(&a.address).map_or(0, |s|s.votes.len())
        })).collect();
        let active_challenges = engine.active_challenges().len();
        let challenges: Vec<_> = engine.active_challenges().take(50).map(|c|serde_json::json!({
            "challenger":c.challenger.as_str(),"target":c.target.as_str(),"stake":c.stake_amount.to_string(),"status":"active","timestamp":c.initiated_at.as_secs()
        })).collect();
        drop(engine);
        Ok(serde_json::json!({
            "active_verifications":active_verifications,"pending_endorsements":pending_endorsements,
            "active_challenges":active_challenges,"verifier_pool_size":verifier_pool_size,
            "total_verified":total_verified,"total_revoked":total_revoked,
            "recent_verifications":verified,"recent_endorsements":endorsements,"recent_challenges":challenges
        }))
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recent_matches_stable_sort_with_bounded_retention() {
        for limit in [0, 1, 50, 500] {
            let rows: Vec<_> = (0..4096).map(|i| ((i * 37 % 113) as u64, i)).collect();
            let mut expected = rows.clone();
            expected.sort_by_key(|(at, _)| Reverse(*at));
            expected.truncate(limit);
            let mut recent = Recent::new(limit);
            for row in rows {
                recent.push(row.0, || row);
                assert!(recent.entries.len() <= limit);
            }
            assert_eq!(recent.values().collect::<Vec<_>>(), expected);
        }
    }
    #[test]
    fn discarded_rows_are_not_projected() {
        let mut recent = Recent::new(2);
        recent.push(10, || 1);
        recent.push(10, || 2);
        recent.push(10, || {
            panic!("later equal timestamp must not replace earlier row")
        });
        recent.push(9, || panic!("older row must not be projected"));
        assert_eq!(recent.values().collect::<Vec<_>>(), vec![1, 2]);
    }
}
