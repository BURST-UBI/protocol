//! Borrow complete constitutional records, without filtering by phase or age.
use super::*;

impl ConstiEngine {
    /// Keep the iterator between batches. This neither copies records nor sorts
    /// them; traversal has the existing HashMap capacity cost and native order.
    pub fn retained_amendments(&self) -> impl Iterator<Item = (&TxHash, &Amendment)> {
        self.amendments.iter()
    }

    /// Preserve even empty buckets. Callers can resume each borrowed vote map's
    /// iterator separately without collecting a ledger-sized vector.
    pub fn retained_vote_buckets(&self) -> impl Iterator<Item = (&TxHash, &HashMap<WalletAddress, ConstiVote>)> {
        self.votes.iter()
    }

    pub fn retained_conflicts(&self) -> impl Iterator<Item = (&TxHash, &AmendmentConflict)> {
        self.conflicts.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::Timestamp;

    #[test]
    fn constitutional_retention_preserves_all_phases_votes_conflicts_and_document() {
        let mut engine = ConstiEngine::bootstrap();
        let phases = [GovernancePhase::Proposal, GovernancePhase::Exploration,
            GovernancePhase::Cooldown, GovernancePhase::Promotion, GovernancePhase::Activation,
            GovernancePhase::Rejected, GovernancePhase::Activated, GovernancePhase::Withdrawn,
            GovernancePhase::Conflicted];
        for (number, phase) in phases.into_iter().enumerate() {
            let hash = TxHash::new([number as u8 + 1;32]);
            // Physical record coverage; not a claim that arbitrary phase jumps
            // are accepted by governance transition validation.
            engine.amendments.insert(hash, Amendment { hash,
                proposer: WalletAddress::new("brst_author"), title: format!("Amendment {number}"),
                text: "Retained text".into(), phase, votes_yea: 1, votes_nay: 2, votes_abstain: 3,
                created_at: Timestamp::new(100 + number as u64), operations: vec![] });
            engine.votes.insert(hash, if number == 0 { HashMap::new() } else {
                [(WalletAddress::new("brst_yes"), ConstiVote::Yea),
                 (WalletAddress::new("brst_no"), ConstiVote::Nay),
                 (WalletAddress::new("brst_abstain"), ConstiVote::Abstain)].into_iter().collect()
            });
        }
        engine.conflicts.insert(TxHash::new([9;32]), AmendmentConflict::ArticleNotFound(77));
        engine.document.articles[0].repealed = true;
        engine.document.articles[0].introduced_by_amendment = 42;
        engine.document.version_history.push(VersionEntry { version: 42, description: "Retained version".into() });
        let before = bincode::serialize(&engine).unwrap();
        assert_eq!(before, bincode::serialize(&engine.snapshot_view()).unwrap());
        assert_eq!(bincode::serialize(&engine.document).unwrap(),
            bincode::serialize(&engine.document.snapshot_view()).unwrap());
        let mut candidate = engine.clone();
        let mut amendments = engine.retained_amendments();
        let first = amendments.next().unwrap();
        assert!(std::ptr::eq(first.1, engine.get_amendment(first.0).unwrap()));
        candidate.get_amendment_mut(first.0).unwrap().text = "Other branch".into();
        assert_eq!(first.1.text, "Retained text");
        assert_eq!(amendments.by_ref().take(2).count(),2);
        assert_eq!(amendments.count(),6);
        assert_eq!(engine.retained_vote_buckets().count(),9);
        assert_eq!(engine.retained_vote_buckets().filter(|(_, votes)| votes.is_empty()).count(),1);
        assert_eq!(engine.retained_vote_buckets().map(|(_, votes)| votes.len()).sum::<usize>(),24);
        assert_eq!(engine.retained_conflicts().collect::<Vec<_>>(),
            vec![(&TxHash::new([9;32]), &AmendmentConflict::ArticleNotFound(77))]);
        let document = engine.get_constitution();
        assert!(document.articles[0].repealed);
        assert_eq!(document.articles[0].introduced_by_amendment,42);
        assert_eq!(document.version_history.last().unwrap().version,42);
        assert_eq!(before,bincode::serialize(&engine).unwrap());
    }
}
