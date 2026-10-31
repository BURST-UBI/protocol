//! Whole-state row export and reconstruction from a sorted storage view.
//! The caller pins that view and verifies its provenance and completeness.
use super::*;
use super::super::visitor::RecordSink;

/// Deterministic full export; ordinary transitions use scoped deltas.
/// Buffers one encoded target at a time, plus sets of target and verifier keys.
pub fn visit_state_rows(
    engine: &VerificationOrchestrator,
    mut visit: impl FnMut(String, Vec<u8>) -> Result<(), String>,
) -> Result<(), String> {
    let (key, bytes) = engine.rules_record().storage_entry()?;
    visit(key, bytes)?;
    let verifiers: BTreeSet<_> = engine.penalized_verifiers.keys().map(WalletAddress::as_str)
        .chain(engine.neither_tracker.history.keys().map(String::as_str)).collect();
    for address in verifiers {
        let record = engine.verifier_record(&WalletAddress::new(address))
            .ok_or("missing verification verifier record")?;
        let (key, bytes) = record.storage_entry()?;
        visit(key, bytes)?;
    }
    let targets: BTreeSet<_> = engine.states.keys().chain(engine.active_challenges.keys())
        .chain(engine.active_appeals.keys()).chain(engine.prior_votes.keys())
        .chain(engine.selections.keys()).chain(engine.attempts.targets()).collect();
    for address in targets {
        let record = engine.target_record(address).ok_or("missing verification target record")?;
        for (key, bytes) in target_rows(&record)? { visit(key, bytes)?; }
    }
    Ok(())
}

pub(super) fn address_from_key(key: &str) -> Result<WalletAddress, String> {
    let (encoded, _) = key.strip_prefix(PREFIX).and_then(|s| s.split_once('/'))
        .ok_or("invalid normalized target key")?;
    if encoded.len() % 2 != 0 || key.len() > 511 { return Err("invalid normalized target key".into()); }
    let bytes = encoded.as_bytes().chunks_exact(2).map(|pair| {
        let text = std::str::from_utf8(pair).map_err(|_| "invalid normalized address encoding")?;
        u8::from_str_radix(text, 16).map_err(|_| "invalid normalized address encoding")
    }).collect::<Result<Vec<_>, _>>()?;
    let address = WalletAddress::new(std::str::from_utf8(&bytes).map_err(|_| "invalid normalized address encoding")?);
    if !key.starts_with(&root(&address)?) { return Err("noncanonical normalized address key".into()); }
    Ok(address)
}

/// Read globally sorted rows from one pinned storage cut. Members are decoded
/// directly into their target; encoded row bytes are never retained by this reader.
/// The supplied budget counts the canonical snapshot, not physical row framing.
struct RowReader<S> {
    logical: S,
    active: Option<(WalletAddress, TargetReader)>,
    last_key: Option<String>,
    max_snapshot_bytes: u64,
    failed: bool,
}
impl<S: RecordSink> RowReader<S> {
    fn new(logical: S, max_snapshot_bytes: u64) -> Self {
        Self { logical, active: None,
            last_key: None, max_snapshot_bytes, failed: false }
    }
    fn flush(&mut self) -> Result<(), String> {
        if let Some((_, target)) = self.active.take() {
            self.logical.accept(target.finish()?, self.max_snapshot_bytes, false)?;
        }
        Ok(())
    }
    pub fn push(&mut self, key: &str, bytes: &[u8]) -> Result<(), String> {
        if self.failed { return Err("normalized verification reader already failed".into()); }
        let result = self.push_inner(key, bytes);
        self.failed |= result.is_err();
        result
    }
    fn push_inner(&mut self, key: &str, bytes: &[u8]) -> Result<(), String> {
        if key.len() > 511 || self.last_key.as_deref().is_some_and(|last| key <= last) {
            return Err("normalized verification rows are not strictly ordered".into());
        }
        if key.starts_with(PREFIX) {
            let address = address_from_key(key)?;
            if self.active.as_ref().is_none_or(|(current, _)| *current != address) {
                self.flush()?;
                if key != format!("{}0", root(&address)?) { return Err("missing normalized target header".into()); }
                let retained = self.logical.retained_bytes()?;
                let remaining = self.max_snapshot_bytes.checked_sub(retained)
                    .ok_or("verification snapshot exceeds byte limit")?;
                self.active = Some((address.clone(), TargetReader::new(&address, bytes, remaining)?));
            } else {
                self.active.as_mut().unwrap().1.push(key, bytes)?;
            }
        } else {
            self.flush()?;
            let record = VerificationRecord::from_storage_entry(key, bytes)?;
            if matches!(record.0, Record::Target(_)) { return Err("aggregate target in normalized state".into()); }
            self.logical.accept(record, self.max_snapshot_bytes, true)?;
        }
        self.last_key = Some(key.to_owned());
        Ok(())
    }
    fn finish(mut self) -> Result<S, String> {
        if self.failed { return Err("normalized verification reader already failed".into()); }
        self.flush()?;
        Ok(self.logical)
    }
}

/// Reconstruct an engine from strictly ordered rows. The budget limits canonical
/// snapshot bytes, not total allocation. A rejected row prevents completion.
pub struct StateReader(RowReader<VerificationRecordReader>);
impl StateReader {
    pub fn new(limit: u64) -> Self { Self(RowReader::new(VerificationRecordReader::default(), limit)) }
    pub fn push(&mut self, key: &str, bytes: &[u8]) -> Result<(), String> { self.0.push(key, bytes) }
    pub fn finish(self) -> Result<VerificationOrchestrator, String> { self.0.finish()?.finish() }
}

/// Stream decoded targets with the same row checks and byte accounting.
/// Earlier callbacks are not undone if a later row or `finish` fails.
pub struct StateVisitor<'a>(RowReader<VerificationRecordVisitor<'a>>);
impl<'a> StateVisitor<'a> {
    pub fn new(limit: u64, visit: &'a mut dyn FnMut(&str, &OrchestratorSnapshot) -> Result<(), String>) -> Self {
        Self(RowReader::new(VerificationRecordVisitor::new(visit), limit))
    }
    pub fn push(&mut self, key: &str, bytes: &[u8]) -> Result<(), String> { self.0.push(key, bytes) }
    pub fn finish(self) -> Result<(), String> { self.0.finish()?.finish() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_state_reader_rejects_mixed_missing_duplicate_and_unordered_rows() {
        let mut engine = VerificationOrchestrator::new();
        let params = ProtocolParams { endorsement_threshold: 1, ..ProtocolParams::default() };
        let target = WalletAddress::new("brst_normalized_target");
        engine.process_endorsement_at(&target, &WalletAddress::new("brst_endorser"),
            params.endorsement_burn_amount, &params, Timestamp::new(1)).unwrap();
        engine.drain_events();
        let mut rows = Vec::new();
        visit_state_rows(&engine, |key, bytes| { rows.push((key, bytes)); Ok(()) }).unwrap();
        for case in 0..7 {
            let mut bad = rows.clone();
            match case {
                0 => { bad.remove(0); }
                1 => { bad.insert(1, bad[0].clone()); }
                2 => { bad.reverse(); }
                3 => {
                    bad.push(engine.target_record(&target).unwrap().storage_entry().unwrap());
                    bad.sort_by(|a, b| a.0.cmp(&b.0));
                }
                4 => { let member = bad.iter().position(|(key, _)| key.contains("/1/0/")).unwrap(); bad.remove(member); }
                5 => { bad[0].1.push(0); }
                _ => { bad[0].1[0] = 2; }
            }
            let mut reader = StateReader::new(64 << 20);
            let mut rejected = false;
            let mut callback = |_: &str, _: &OrchestratorSnapshot| Ok(());
            let mut visitor = StateVisitor::new(64 << 20, &mut callback);
            for (key, bytes) in &bad {
                if visitor.push(key, bytes).is_err() { break; }
            }
            assert!(visitor.finish().is_err(), "visitor case {case}");
            for (key, bytes) in bad {
                if reader.push(&key, &bytes).is_err() { rejected = true; break; }
            }
            if rejected { assert!(reader.push(&rows[0].0, &rows[0].1).is_err()); }
            assert!(reader.finish().is_err(), "case {case}");
        }
    }
}
