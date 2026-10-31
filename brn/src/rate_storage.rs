//! Local storage for rate history: a count header and indexed start/rate rows.
//! Segment ends are reconstructed from the next row's start. This layout is
//! separate from the segment sequence serialized by `RateHistory`.

use crate::{BrnError, RateHistory, RateSegment};
use burst_types::Timestamp;

pub const HEADER: &[u8] = b"rate_history/v1/count";
pub const ROW_PREFIX: &[u8] = b"rate_history/v1/segment/";

fn invalid() -> BrnError {
    BrnError::Other("invalid persisted BRN rate rows".into())
}

/// Append a big-endian sequence index to the rate-row prefix.
pub fn row_key(index: u64) -> Vec<u8> {
    let mut key = Vec::with_capacity(ROW_PREFIX.len() + 8);
    key.extend_from_slice(ROW_PREFIX);
    key.extend_from_slice(&index.to_be_bytes());
    key
}

/// The next row's start closes this row. An append therefore leaves every
/// previous encoded row unchanged, including zero-duration rate changes.
pub fn row_value(segment: &RateSegment) -> [u8; 24] {
    let mut bytes = [0; 24];
    bytes[..8].copy_from_slice(&segment.start.as_secs().to_be_bytes());
    bytes[8..].copy_from_slice(&segment.rate.to_be_bytes());
    bytes
}

/// Full replacement must reject malformed segment ends instead of silently
/// repairing them by omitting those ends from the local row representation.
pub fn encode(history: &RateHistory) -> Result<impl Iterator<Item = (Vec<u8>, [u8; 24])> + '_, BrnError> {
    let mut segments = history.segments().iter().peekable();
    if segments.peek().is_none() { return Err(invalid()); }
    while let Some(segment) = segments.next() {
        match segments.peek() {
            Some(next) if segment.end == Some(next.start) && next.start >= segment.start => {},
            None if segment.end.is_none() => {},
            _ => return Err(invalid()),
        }
    }
    Ok(history.segments().iter().enumerate()
        .map(|(index, segment)| (row_key(index as u64), row_value(segment))))
}

/// Read either the complete row format or a single Bincode history value.
/// Reject mixed formats and orphaned rows. The Bincode branch reconstructs the
/// history without validating segment boundaries; engine restoration checks them.
pub fn decode_stored<K: AsRef<[u8]>, V: AsRef<[u8]>>(
    legacy: Option<&[u8]>,
    header: Option<&[u8]>,
    rows: impl IntoIterator<Item = Result<(K, V), BrnError>>,
) -> Result<Option<RateHistory>, BrnError> {
    let mut rows = rows.into_iter();
    match header {
        Some(header) => {
            if legacy.is_some() { return Err(invalid()); }
            decode(header, rows).map(Some)
        }
        None => {
            if rows.next().transpose()?.is_some() { return Err(invalid()); }
            legacy.map(|bytes| bincode::deserialize(bytes)
                .map_err(|error| BrnError::Other(error.to_string()))).transpose()
        }
    }
}

pub fn read_snapshot(snapshot: &dyn burst_store::brn::BrnSnapshot) -> Result<Option<RateHistory>, BrnError> {
    let error = |error: burst_store::StoreError| BrnError::Other(error.to_string());
    decode_stored(snapshot.get_meta(b"rate_history").map_err(error)?,
        snapshot.get_meta(HEADER).map_err(error)?,
        snapshot.meta_entries(ROW_PREFIX).map_err(error)?
            .map(|row| row.map_err(error)))
}

/// Require exactly the declared nonempty sequence of rows, starting at index
/// zero, with nondecreasing timestamps. Equal starts allow zero-duration rates.
/// No capacity is allocated from the header count; memory grows with actual rows.
pub fn decode<K: AsRef<[u8]>, V: AsRef<[u8]>>(
    header: &[u8],
    rows: impl IntoIterator<Item = Result<(K, V), BrnError>>,
) -> Result<RateHistory, BrnError> {
    let count = u64::from_be_bytes(header.try_into().map_err(|_| invalid())?);
    if count == 0 { return Err(invalid()); }
    let mut segments: Vec<RateSegment> = Vec::new();
    let mut expected = 0u64;
    for row in rows {
        let (key, value) = row?;
        let key = key.as_ref();
        let value = value.as_ref();
        if expected >= count || key.strip_prefix(ROW_PREFIX) != Some(expected.to_be_bytes().as_slice()) || value.len() != 24 {
            return Err(invalid());
        }
        let start = Timestamp::new(u64::from_be_bytes(value[..8].try_into().unwrap()));
        let rate = u128::from_be_bytes(value[8..].try_into().unwrap());
        if let Some(previous) = segments.last_mut() {
            if start < previous.start { return Err(invalid()); }
            previous.end = Some(start);
        }
        segments.push(RateSegment { rate, start, end: None });
        expected = expected.checked_add(1).ok_or_else(invalid)?;
    }
    if expected != count { return Err(invalid()); }
    Ok(RateHistory::from_segments(segments))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(history: &RateHistory) -> Vec<(Vec<u8>, Vec<u8>)> {
        encode(history).unwrap()
            .map(|(key, value)| (key, value.to_vec()))
            .collect()
    }

    fn restore(count: u64, rows: Vec<(Vec<u8>, Vec<u8>)>) -> Result<RateHistory, BrnError> {
        decode(&count.to_be_bytes(), rows.into_iter().map(Ok))
    }

    #[test]
    fn row_append_preserves_prefix_and_legacy_snapshot_encoding() {
        let mut before = RateHistory::new(u128::MAX, Timestamp::EPOCH);
        before.apply_rate_change(7, Timestamp::new(10)).unwrap();
        let baseline = rows(&before);
        let mut after = before.clone();
        after.apply_rate_change(0, Timestamp::new(10)).unwrap();
        after.apply_rate_change(11, Timestamp::new(u64::MAX)).unwrap();
        let appended = after.appended_rows_start(&before).unwrap();
        assert_eq!(appended, 2);
        let encoded = rows(&after);
        assert_eq!(&encoded[..appended], &baseline);
        let restored = restore(4, encoded.clone()).unwrap();
        assert_eq!(bincode::serialize(&restored).unwrap(), bincode::serialize(&after).unwrap());
        assert_eq!(restored.total_accrued_checked(Timestamp::new(10), Timestamp::new(u64::MAX)), Some(0));
        // Deleting appended rows and restoring the count reconstructs the old
        // open final segment; no previous rate row needs rewriting on undo.
        assert_eq!(restore(2, encoded[..2].to_vec()).unwrap(), before);
    }

    #[test]
    fn append_identity_does_not_trust_an_equal_or_replaced_history() {
        let before = RateHistory::new(3, Timestamp::EPOCH);
        let clone = before.clone();
        assert_eq!(clone.appended_rows_start(&before), Some(1));
        let mut independent = RateHistory::new(3, Timestamp::EPOCH);
        independent.apply_rate_change(5, Timestamp::new(10)).unwrap();
        assert_eq!(independent.appended_rows_start(&before), None);
        let mut after = before.clone();
        after.apply_rate_change(5, Timestamp::new(10)).unwrap();
        assert_eq!(after.appended_rows_start(&before), Some(1));
        let mut next = after.clone();
        next.apply_rate_change(8, Timestamp::new(11)).unwrap();
        assert_eq!(next.appended_rows_start(&after), Some(2));
        assert_eq!(next.appended_rows_start(&before), None);
        let restored: RateHistory = bincode::deserialize(&bincode::serialize(&after).unwrap()).unwrap();
        assert_eq!(restored.appended_rows_start(&before), None);
        assert_eq!(after.appended_rows_start(&before), Some(1));
    }

    #[test]
    fn rate_rows_reject_missing_extra_misordered_and_malformed_inventory() {
        let mut history = RateHistory::new(3, Timestamp::new(10));
        history.apply_rate_change(7, Timestamp::new(20)).unwrap();
        let valid = rows(&history);
        assert!(restore(0, vec![]).is_err());
        assert!(restore(u64::MAX, vec![]).is_err());
        assert!(restore(3, valid.clone()).is_err());
        assert!(restore(1, valid.clone()).is_err());
        for length in [0, 7, 9] {
            assert!(decode(&vec![0; length], valid.clone().into_iter().map(Ok)).is_err());
        }
        let mut invalid = valid.clone();
        invalid.reverse();
        assert!(restore(2, invalid).is_err());
        for key in [vec![], row_key(2), [row_key(1), vec![0]].concat()] {
            let mut invalid = valid.clone();
            invalid[1].0 = key;
            assert!(restore(2, invalid).is_err());
        }
        for length in [0, 23, 25] {
            let mut invalid = valid.clone();
            invalid[1].1 = vec![0; length];
            assert!(restore(2, invalid).is_err());
        }
        let mut invalid = valid.clone();
        invalid[1].1[..8].copy_from_slice(&9u64.to_be_bytes());
        assert!(restore(2, invalid).is_err());
        assert!(decode(&2u64.to_be_bytes(), valid.into_iter().map(Ok)
            .chain(std::iter::once(Err::<(Vec<u8>, Vec<u8>), _>(super::invalid())))).is_err());
    }

    #[test]
    fn full_encoding_does_not_repair_malformed_segment_ends() {
        assert!(encode(&RateHistory::from_segments(vec![])).is_err());
        let mut history = RateHistory::new(3, Timestamp::new(10));
        history.apply_rate_change(5, Timestamp::new(20)).unwrap();
        let valid: Vec<_> = history.segments().iter().cloned().collect();
        for (index, end) in [(0, None), (0, Some(Timestamp::new(19))), (1, Some(Timestamp::new(30)))] {
            let mut invalid = valid.clone();
            invalid[index].end = end;
            assert!(encode(&RateHistory::from_segments(invalid)).is_err());
        }
        let mut invalid = valid;
        invalid[1].start = Timestamp::new(9);
        invalid[0].end = Some(Timestamp::new(9));
        assert!(encode(&RateHistory::from_segments(invalid)).is_err());
    }
}
