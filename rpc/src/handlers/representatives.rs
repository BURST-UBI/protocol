use burst_types::WalletAddress;

/// Collect all weights, partition around the requested offset, then sort that page.
/// Memory grows with the full representative set, even for a small page. Ties keep
/// input order within this snapshot; offsets do not pin order across later snapshots.
pub(super) fn page<'a>(
    weights: impl Iterator<Item = (&'a WalletAddress, u128)>,
    offset: u64,
    count: u32,
) -> (Vec<(&'a WalletAddress, u128)>, Option<u64>) {
    let mut rows: Vec<_> = weights.enumerate().collect();
    let start = usize::try_from(offset).unwrap_or(usize::MAX).min(rows.len());
    let end = start.saturating_add(count as usize).min(rows.len());
    let cursor = (end < rows.len()).then_some(end as u64);
    if start == end {
        return (Vec::new(), cursor);
    }
    // The ordinal preserves the stable descending-weight sort for this snapshot,
    // including ties. Partition away other pages, then sort only this page.
    let compare = |a: &(usize, (&WalletAddress, u128)), b: &(usize, (&WalletAddress, u128))| {
        b.1.1.cmp(&a.1.1).then_with(|| a.0.cmp(&b.0))
    };
    if start > 0 {
        rows.select_nth_unstable_by(start, compare);
    }
    let tail = &mut rows[start..];
    let size = end - start;
    if size < tail.len() {
        tail.select_nth_unstable_by(size, compare);
    }
    let selected = &mut tail[..size];
    selected.sort_unstable_by(compare);
    (selected.iter().map(|(_, row)| *row).collect(), cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_matches_stable_full_sort_for_ties_offsets_and_boundaries() {
        let addresses: Vec<_> = (0..257).map(|n| WalletAddress::new(format!("brst_rep{n}"))).collect();
        for length in [0, 1, 20, 257] {
            let weights: Vec<_> = addresses[..length].iter().enumerate()
                .map(|(n, address)| (address, ((n * 37) % 11) as u128)).collect();
            let mut expected = weights.clone();
            expected.sort_by_key(|(_, weight)| std::cmp::Reverse(*weight));
            for offset in [0, 1, 19, 20, 255, 257, u64::MAX] {
                for count in [0, 1, 20, 100, 1000] {
                    let start = usize::try_from(offset).unwrap_or(usize::MAX).min(length);
                    let end = start.saturating_add(count as usize).min(length);
                    let (actual, next) = page(weights.iter().copied(), offset, count);
                    assert_eq!(actual, expected[start..end], "length={length} offset={offset} count={count}");
                    assert_eq!(next, (end < length).then_some(end as u64));
                }
            }
        }
    }
}
