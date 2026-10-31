use super::{Candidate, CandidateRows, ScratchRecords, SortedRecordsBuilder, BlockHash, StoreError, Path, invalid};
use crate::confirmed_replay::snapshot::installed::{BodyObservations, BodyObservationPages};

// Hashes and lengths already belong to immutable checked observation pages.
// Only classification flags are new: one byte per body, with one authenticated
// record per original page. The two indexes use the same last-hash page keys.
pub(crate) struct ObservedCatalogue {
    bodies: BodyObservations,
    flags: ScratchRecords<Vec<u8>>,
}

impl CandidateRows {
    pub(crate) fn classify(
        bodies: BodyObservations, directory: &Path, max_rows: usize, max_bytes: u64,
        mut classify: impl FnMut(BlockHash, u64) -> Result<Candidate, StoreError>,
    ) -> Result<Self, StoreError> {
        if bodies.count() > max_rows as u64
            || bodies.count().checked_mul(10).is_none_or(|bytes| bytes > max_bytes) {
            return Err(invalid("retirement catalogue exceeds inventory budget"));
        }
        // Every page contains at least one body; actual extents share the same
        // ScratchLimit as the source observations, roots and other capture work.
        let mut builder = SortedRecordsBuilder::new(directory, max_rows, max_bytes)?;
        let mut count = 0u64;
        for page in bodies.pages(None)? {
            let (key, rows) = page?;
            let mut flags = Vec::with_capacity(rows.len());
            for (hash, length) in rows {
                let candidate = classify(hash, length)?;
                if candidate.body_bytes != length { return Err(invalid("retirement classification changed body length")); }
                let encoded = candidate.encode();
                Candidate::decode(encoded)?;
                flags.push(encoded[8]);
                count = count.checked_add(1).filter(|count| *count <= bodies.count())
                    .ok_or_else(|| invalid("retirement observation count overflow"))?;
            }
            builder.push(key, &flags)?;
        }
        if count != bodies.count() { return Err(invalid("retirement observation count differs")); }
        Ok(Self::Observed(ObservedCatalogue { bodies, flags: builder.finish()? }))
    }
}

impl ObservedCatalogue {
    pub(super) fn scan(&self, after: Option<BlockHash>) -> Result<ObservedRows, StoreError> {
        Ok(ObservedRows { pages: self.bodies.pages(after)?, flags: self.flags.fork()?,
            pending: Vec::new().into_iter(), after, failed: false })
    }
    #[cfg(test)]
    pub(super) fn truncate_for_test(&self) { self.flags.truncate_for_test(); }
}

pub(crate) struct ObservedRows {
    pages: BodyObservationPages,
    flags: ScratchRecords<Vec<u8>>,
    pending: std::vec::IntoIter<(BlockHash, Candidate)>,
    after: Option<BlockHash>,
    failed: bool,
}
impl Iterator for ObservedRows {
    type Item = Result<(BlockHash, Candidate), StoreError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed { return None; }
        loop {
            for (hash, candidate) in self.pending.by_ref() {
                if self.after.is_none_or(|after| hash > after) { return Some(Ok((hash, candidate))); }
            }
            let page = self.pages.next()?;
            let result = (|| {
                let (key, rows) = page?;
                let flags = self.flags.get(&key)?.ok_or_else(|| invalid("retirement page classification is missing"))?;
                if flags.len() != rows.len() { return Err(invalid("retirement page classification length differs")); }
                // Check the entire page, including entries before a resume cut.
                let candidates = rows.into_iter().zip(flags).map(|((hash, length), flags)| {
                    let mut encoded = [0; 32];
                    encoded[..8].copy_from_slice(&length.to_be_bytes());
                    encoded[8] = flags;
                    Candidate::decode(encoded).map(|candidate| (hash, candidate))
                }).collect::<Result<Vec<_>, StoreError>>()?;
                self.pending = candidates.into_iter();
                Ok(())
            })();
            if let Err(error) = result { self.failed = true; return Some(Err(error)); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confirmed_replay::scratch_file::{ScratchFile, ScratchLimit};
    fn hash(n: u64) -> BlockHash { let mut key = [0; 32]; key[24..].copy_from_slice(&n.to_be_bytes()); BlockHash::new(key) }
    fn candidate(hash: BlockHash, length: u64) -> Candidate {
        let n = u64::from_be_bytes(hash.as_bytes()[24..].try_into().unwrap());
        Candidate { body_bytes: length, in_snapshot_prefix: n % 3 != 0, eligible: n % 3 == 1 }
    }

    #[test]
    fn shared_observations_preserve_lookup_resume_and_reject_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let limit = ScratchLimit::acquire(directory.path(), 1 << 20).unwrap();
        let bodies = BodyObservations::fixture(directory.path(), (0..131).map(|n| (hash(n * 2), n as usize + 17)));
        let rows = CandidateRows::classify(bodies, directory.path(), 131, 1310,
            |hash, length| Ok(candidate(hash, length))).unwrap();
        for n in 0..=263 {
            assert_eq!(rows.get(hash(n).as_bytes()).unwrap(),
                (n <= 260 && n % 2 == 0).then(|| candidate(hash(n), n / 2 + 17)));
            let tail = rows.scan(Some(hash(n))).unwrap().map(|row| row.unwrap().0).collect::<Vec<_>>();
            assert_eq!(tail, (0..131).map(|n| hash(n * 2)).filter(|hash| *hash > self::hash(n)).collect::<Vec<_>>());
        }
        // Retain an already created cursor: corruption must still be detected.
        let mut cursor = rows.scan(Some(hash(1))).unwrap();
        rows.truncate_for_test();
        assert!(cursor.next().unwrap().is_err());
        assert!(cursor.next().is_none());
        drop(cursor); drop(rows);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);

        let bodies = BodyObservations::fixture(directory.path(), (0..131).map(|n| (hash(n), 17)));
        let mut rows = CandidateRows::classify(bodies, directory.path(), 131, 1310,
            |hash, length| Ok(candidate(hash, length))).unwrap();
        let CandidateRows::Observed(shared) = &mut rows else { unreachable!() };
        // Validly hashed malformed flags still fail, including skipped rows.
        shared.flags.put(*hash(63).as_bytes(), &vec![1; 64]).unwrap();
        assert!(rows.scan(Some(hash(10))).unwrap().next().unwrap().is_err());
        let CandidateRows::Observed(shared) = &mut rows else { unreachable!() };
        shared.bodies.truncate_for_test();
        assert!(rows.get(hash(130).as_bytes()).is_err());
        drop(rows);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
    }

    #[test]
    fn shared_catalogue_fits_retained_capture_headroom() {
        let directory = tempfile::tempdir().unwrap();
        let limit = ScratchLimit::acquire(directory.path(), 50_266_112).unwrap();
        let bodies = BodyObservations::fixture(directory.path(), (0..180_229).map(|n| (hash(n), 512)));
        // Observations are part of the measured resident recovery inventory.
        let mut resident = ScratchFile::new_in(directory.path()).unwrap();
        resident.set_len(41_390_076 - limit.used_temporary_bytes().unwrap()).unwrap();
        let mut roots = ScratchFile::new_in(directory.path()).unwrap();
        roots.set_len(6_666_130).unwrap();
        let before = limit.used_temporary_bytes().unwrap();
        let rows = CandidateRows::classify(bodies, directory.path(), 180_229, 50_266_112,
            |hash, length| Ok(candidate(hash, length))).unwrap();
        let extra = limit.used_temporary_bytes().unwrap() - before;
        assert!(extra < 1_000_000, "classification must share existing hash/length pages: {extra}");
        let mut count = 0;
        for row in rows.scan(None).unwrap() {
            let (hash, row) = row.unwrap();
            assert_eq!(row, candidate(hash, 512));
            count += 1;
        }
        assert_eq!(count, 180_229);
        drop(resident); drop(roots);
        assert_eq!(rows.get(hash(180_228).as_bytes()).unwrap(), Some(candidate(hash(180_228), 512)));
        drop(rows);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
    }

    #[test]
    fn shared_catalogue_rejects_partial_classification_and_changed_lengths() {
        let directory = tempfile::tempdir().unwrap();
        let limit = ScratchLimit::acquire(directory.path(), 1 << 20).unwrap();
        for stop in [0, 63, 64, 130] {
            let bodies = BodyObservations::fixture(directory.path(), (0..131).map(|n| (hash(n), 17)));
            assert!(CandidateRows::classify(bodies, directory.path(), 131, 1310, |key, length| {
                if key == hash(stop) { return Err(invalid("injected cancellation")); }
                Ok(candidate(key, length))
            }).is_err());
            assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
        }
        for (max_rows, max_bytes, changed_length) in [(130, 1310, false), (131, 1309, false), (131, 1310, true)] {
            let bodies = BodyObservations::fixture(directory.path(), (0..131).map(|n| (hash(n), 17)));
            assert!(CandidateRows::classify(bodies, directory.path(), max_rows, max_bytes,
                |key, length| Ok(candidate(key, length + u64::from(changed_length)))).is_err());
            assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
        }
    }
}
