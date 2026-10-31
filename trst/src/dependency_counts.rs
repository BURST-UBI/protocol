//! Checked dependency counts: persistent for mutation, transient for validation.
use crate::SharedMap;
use burst_types::TxHash;
use std::collections::HashMap;

pub(crate) trait Counts: Default {
    fn get_count(&self, key: &TxHash) -> usize;
    fn set_count(&mut self, key: TxHash, count: usize);
}

impl Counts for SharedMap<TxHash, usize> {
    fn get_count(&self, key: &TxHash) -> usize { self.get(key).copied().unwrap_or(0) }
    fn set_count(&mut self, key: TxHash, count: usize) {
        if count == 0 { self.remove(&key); } else { self.insert(key, count); }
    }
}

impl Counts for HashMap<TxHash, usize> {
    fn get_count(&self, key: &TxHash) -> usize { self.get(key).copied().unwrap_or(0) }
    fn set_count(&mut self, key: TxHash, count: usize) {
        if count == 0 { self.remove(&key); } else { self.insert(key, count); }
    }
}

/// Change one count, removing zero entries. Overflow or underflow leaves that
/// entry unchanged; callers stage multi-entry updates when they need atomicity.
pub(crate) fn adjust<C: Counts>(counts: &mut C, key: TxHash, add: bool, error: &'static str) -> Result<(), String> {
    let old = counts.get_count(&key);
    let next = if add { old.checked_add(1) } else { old.checked_sub(1) }.ok_or(error)?;
    counts.set_count(key, next);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transient_and_persistent_counts_keep_exact_overflow_and_underflow_rules() {
        let key = TxHash::new([1; 32]);
        let mut persistent = SharedMap::default();
        let mut transient = HashMap::default();
        for initial in [0, 1, usize::MAX - 1, usize::MAX] {
            for add in [false, true] {
                persistent.set_count(key, initial);
                transient.set_count(key, initial);
                assert_eq!(adjust(&mut persistent, key, add, "count failure"),
                    adjust(&mut transient, key, add, "count failure"));
                assert_eq!(persistent.get(&key), transient.get(&key));
                if (!add && initial == 0) || (add && initial == usize::MAX) {
                    assert_eq!(persistent.get_count(&key), initial);
                }
            }
        }
    }
}
