use burst_consensus::ActiveElections;
use burst_types::BlockHash;

const REQUEST_LIMIT: usize = 8;
const SCAN_LIMIT: usize = 256;

pub(super) fn select(
    elections: &mut ActiveElections,
    mut historical: Vec<BlockHash>,
    historical_cursor: &mut usize,
) -> Vec<BlockHash> {
    // HistoricalFinality bounds this list to 256 entries. Keep both sources
    // progressing when a large active-election backlog accompanies catch-up.
    historical.sort_unstable();
    historical.dedup();
    if !historical.is_empty() {
        *historical_cursor %= historical.len();
        historical.rotate_left(*historical_cursor);
    }
    let reserved = historical.len().min(REQUEST_LIMIT / 2);
    let mut hashes = historical[..reserved].to_vec();
    for hash in elections.next_unconfirmed_candidates(REQUEST_LIMIT - reserved, SCAN_LIMIT) {
        if !hashes.contains(&hash) { hashes.push(hash); }
    }
    let mut visited = reserved;
    for &hash in &historical[reserved..] {
        if hashes.len() == REQUEST_LIMIT { break; }
        visited += 1;
        if !hashes.contains(&hash) { hashes.push(hash); }
    }
    if !historical.is_empty() {
        *historical_cursor = (*historical_cursor + visited) % historical.len();
    }
    hashes
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::Timestamp;
    use std::collections::BTreeSet;

    #[test]
    fn bounded_requests_rotate_both_sources_and_deduplicate_overlap() {
        let hashes: Vec<_> = (1..=64).map(|n| BlockHash::new([n; 32])).collect();
        let mut elections = ActiveElections::new(64, 1000);
        for &hash in &hashes[..32] {
            elections.register_candidate(hash, hash, Timestamp::new(100)).unwrap();
        }
        let mut cursor = 0;
        let mut seen = BTreeSet::new();
        // Include overlap and duplicate history entries; neither can duplicate
        // a wire request or starve the other source's non-overlapping entries.
        let historical: Vec<_> = hashes[24..].iter().chain(&hashes[24..]).copied().collect();
        for _ in 0..16 {
            let page = select(&mut elections, historical.clone(), &mut cursor);
            assert!(!page.is_empty() && page.len() <= REQUEST_LIMIT);
            assert_eq!(page.iter().copied().collect::<BTreeSet<_>>().len(), page.len());
            seen.extend(page);
        }
        assert_eq!(seen, hashes.iter().copied().collect());
        let mut empty = ActiveElections::new(1, 1000);
        assert_eq!(select(&mut empty, vec![], &mut cursor), vec![]);
        assert_eq!(select(&mut empty, vec![hashes[0]], &mut cursor), vec![hashes[0]]);
        assert_eq!(select(&mut empty, hashes.clone(), &mut cursor).len(), REQUEST_LIMIT);
        assert_eq!(select(&mut elections, vec![], &mut cursor).len(), REQUEST_LIMIT);
    }
}
