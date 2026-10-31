/// One contiguous in-process interval of validated sequence-only commits.
/// Missing, reordered or untracked commits break coverage. No persisted marker
/// can manufacture this proof, and reopening starts with no covered interval.
#[derive(Default)]
pub(crate) struct SequenceCommits {
    interval: Option<(usize, usize)>,
}
impl SequenceCommits {
    pub(crate) fn record(&mut self, before: usize, after: usize) {
        if before.checked_add(1) != Some(after) { self.interval = None; return; }
        let start = self.interval.filter(|(_, end)| *end == before)
            .map_or(before, |(start, _)| start);
        self.interval = Some((start, after));
    }
    pub(crate) fn covers(&self, before: usize, after: usize) -> bool {
        self.interval.is_some_and(|(start, end)| start <= before && before <= after && end == after)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sequence_intervals_require_contiguous_native_commits() {
        let mut commits = SequenceCommits::default();
        assert!(!commits.covers(0, 1));
        for generation in 0..100_000 { commits.record(generation, generation + 1); }
        assert!(commits.covers(0, 100_000));
        assert!(commits.covers(99_999, 100_000));
        assert!(!commits.covers(100_001, 100_000));
        assert!(!commits.covers(0, 100_001));
        // An untracked commit cannot be bridged by later sequence-only work.
        commits.record(100_001, 100_002);
        assert!(!commits.covers(0, 100_002));
        assert!(commits.covers(100_001, 100_002));
        commits.record(100_000, 100_001);
        assert!(!commits.covers(100_001, 100_002));
        commits.record(100_001, 100_003);
        assert!(!commits.covers(100_001, 100_003));
        commits.record(usize::MAX, 0);
        assert!(!commits.covers(usize::MAX, 0));
    }
}
