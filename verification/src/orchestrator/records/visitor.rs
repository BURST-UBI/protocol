//! Stream checked logical records without retaining a complete reconstructed engine.
use super::*;

pub(super) fn empty_snapshot_bytes() -> Result<u64, String> {
    VerificationOrchestrator::new().snapshot().encoded_size().map_err(|e| e.to_string())
}

pub(super) fn snapshot_fragment(record: VerificationRecord, rules: &VerificationRecord,
    limit: u64, physical: bool) -> Result<OrchestratorSnapshot, String> {
    if !matches!(rules.0, Record::Rules { .. }) { return Err("missing verification rules".into()); }
    let mut reader = VerificationRecordReader::default();
    if !matches!(record.0, Record::Rules { .. }) { reader.push_bounded(rules.clone(), limit)?; }
    reader.push_record(record, limit, physical)?;
    Ok(reader.finish()?.snapshot())
}

pub(super) trait RecordSink {
    fn retained_bytes(&self) -> Result<u64, String>;
    fn accept(&mut self, record: VerificationRecord, limit: u64, physical: bool) -> Result<(), String>;
}

impl RecordSink for VerificationRecordReader {
    fn retained_bytes(&self) -> Result<u64, String> {
        self.snapshot_bytes.map(Ok).unwrap_or_else(empty_snapshot_bytes)
    }
    fn accept(&mut self, record: VerificationRecord, limit: u64, physical: bool) -> Result<(), String> {
        self.push_record(record, limit, physical)
    }
}

/// Sorted storage readers supply rules first. The callback sees one validated
/// target/verifier projection at a time; it receives no publication authority.
/// Keys and aggregate accounting are retained, not the prior decoded targets.
/// A later error does not undo earlier callbacks. Consumers must stage effects
/// until `finish` succeeds and independently establish inventory completeness.
pub struct VerificationRecordVisitor<'a> {
    rules: Option<VerificationRecord>,
    keys: HashSet<String>,
    snapshot_bytes: Option<u64>,
    failed: bool,
    visit: &'a mut dyn FnMut(&str, &OrchestratorSnapshot) -> Result<(), String>,
}

impl<'a> VerificationRecordVisitor<'a> {
    pub fn new(visit: &'a mut dyn FnMut(&str, &OrchestratorSnapshot) -> Result<(), String>) -> Self {
        Self { rules: None, keys: HashSet::new(), snapshot_bytes: None, failed: false, visit }
    }
    pub fn push_bounded(&mut self, record: VerificationRecord, limit: u64) -> Result<(), String> {
        self.accept(record, limit, true)
    }
    pub fn finish(self) -> Result<(), String> {
        if self.failed || self.rules.is_none() { return Err("incomplete verification records".into()); }
        Ok(())
    }
}

impl RecordSink for VerificationRecordVisitor<'_> {
    fn retained_bytes(&self) -> Result<u64, String> {
        self.snapshot_bytes.map(Ok).unwrap_or_else(empty_snapshot_bytes)
    }
    fn accept(&mut self, record: VerificationRecord, limit: u64, physical: bool) -> Result<(), String> {
        if self.failed { return Err("verification record visitor already failed".into()); }
        let result = (|| {
            let key = record.key()?;
            if !self.keys.insert(key.clone()) { return Err("duplicate verification record".into()); }
            let next = self.retained_bytes()?.checked_add(record.snapshot_entry_bytes()?)
                .ok_or("verification snapshot size overflow")?;
            if next > limit { return Err("verification snapshot exceeds byte limit".into()); }
            if matches!(record.0, Record::Rules { .. }) { self.rules = Some(record.clone()); }
            let rules = self.rules.as_ref().ok_or("verification rules must precede records")?;
            let snapshot = snapshot_fragment(record, rules, limit, physical)?;
            (self.visit)(&key, &snapshot)?;
            self.snapshot_bytes = Some(next);
            Ok(())
        })();
        self.failed |= result.is_err();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_records_reject_missing_duplicate_invalid_and_callback_failure() {
        let mut state = VerificationOrchestrator::new();
        let address = WalletAddress::new("brst_visitor");
        state.initialize_genesis(&address);
        let rules = state.rules_record();
        let target = state.target_record(&address).unwrap();
        for records in [vec![], vec![target.clone()], vec![rules.clone(), rules.clone()],
            vec![rules.clone(), target.clone(), target.clone()],
            vec![rules.clone(), VerificationRecord(Record::Verifier {
                address: address.clone(), penalty: None, history: None,
            })]] {
            let mut callback = |_: &str, _: &OrchestratorSnapshot| Ok(());
            let mut visitor = VerificationRecordVisitor::new(&mut callback);
            for record in records {
                if visitor.push_bounded(record, 64 << 20).is_err() {
                    assert!(visitor.push_bounded(rules.clone(), 64 << 20).is_err());
                    break;
                }
            }
            assert!(visitor.finish().is_err());
        }
        let mut calls = 0;
        let mut callback = |_: &str, _: &OrchestratorSnapshot| {
            calls += 1;
            Err("cancelled consumer".to_owned())
        };
        let mut visitor = VerificationRecordVisitor::new(&mut callback);
        assert_eq!(visitor.push_bounded(rules.clone(), 64 << 20).unwrap_err(), "cancelled consumer");
        assert!(visitor.push_bounded(target, 64 << 20).is_err());
        assert!(visitor.finish().is_err());
        assert_eq!(calls, 1);

        state.neither_tracker.record_vote(&address, Vote::Neither);
        let invalid_rules = VerificationRecord(Record::Rules { threshold: 5001, window: 0 });
        let verifier = state.verifier_record(&address).unwrap();
        let mut full = VerificationRecordReader::default();
        full.push(invalid_rules.clone()).unwrap();
        full.push(verifier.clone()).unwrap();
        assert!(full.finish().is_err());
        let mut callback = |_: &str, _: &OrchestratorSnapshot| Ok(());
        let mut visitor = VerificationRecordVisitor::new(&mut callback);
        visitor.push_bounded(invalid_rules, 64 << 20).unwrap();
        assert!(visitor.push_bounded(verifier, 64 << 20).is_err());
        assert!(visitor.finish().is_err());
    }
}
