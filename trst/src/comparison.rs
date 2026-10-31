use crate::SharedMap;

pub(crate) type Poll<'a> = &'a dyn Fn(&'static str) -> Result<(), String>;

/// Compare ordered entries, skipping shared storage and identical value pointers.
/// The value callback is an equality check, not a validator: skipped values never
/// reach it. Polling can cancel before the comparison finishes.
pub(crate) fn maps<K: Ord + Clone, V: Clone>(
    left: &SharedMap<K, V>,
    right: &SharedMap<K, V>,
    poll: Poll<'_>,
    stage: &'static str,
    mut equal: impl FnMut(&V, &V) -> Result<bool, String>,
) -> Result<bool, String> {
    poll(stage)?;
    if left.len() != right.len() {
        return Ok(false);
    }
    if left.shares_storage_with(right) {
        return Ok(true);
    }
    for ((key, value), (other_key, other)) in left.iter().zip(right.iter()) {
        poll(stage)?;
        if key != other_key {
            return Ok(false);
        }
        if !std::ptr::eq(value, other) && !equal(value, other)? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn ordered<T: PartialEq>(
    left: impl IntoIterator<Item = T>,
    right: impl IntoIterator<Item = T>,
    poll: Poll<'_>,
    stage: &'static str,
) -> Result<bool, String> {
    let mut left = left.into_iter();
    let mut right = right.into_iter();
    loop {
        poll(stage)?;
        match (left.next(), right.next()) {
            (None, None) => return Ok(true),
            (Some(a), Some(b)) if a == b => {}
            _ => return Ok(false),
        }
    }
}
