use proptest::prelude::*;

use burst_types::{BlockHash, BrnAmount, Timestamp, TrstAmount, TxHash};

proptest! {
    /// BlockHash roundtrip: new -> as_bytes -> new produces identical hash.
    #[test]
    fn block_hash_roundtrip(bytes in prop::array::uniform32(0u8..)) {
        let hash = BlockHash::new(bytes);
        prop_assert_eq!(hash.as_bytes(), &bytes);
    }

    /// TxHash roundtrip: new -> as_bytes -> new produces identical hash.
    #[test]
    fn tx_hash_roundtrip(bytes in prop::array::uniform32(0u8..)) {
        let hash = TxHash::new(bytes);
        prop_assert_eq!(hash.as_bytes(), &bytes);
    }

    /// BlockHash::is_zero is true only for all-zero bytes.
    #[test]
    fn block_hash_is_zero_correct(bytes in prop::array::uniform32(0u8..)) {
        let hash = BlockHash::new(bytes);
        prop_assert_eq!(hash.is_zero(), bytes == [0u8; 32]);
    }

    /// TxHash::is_zero is true only for all-zero bytes.
    #[test]
    fn tx_hash_is_zero_correct(bytes in prop::array::uniform32(0u8..)) {
        let hash = TxHash::new(bytes);
        prop_assert_eq!(hash.is_zero(), bytes == [0u8; 32]);
    }

    /// BlockHash bincode serialization roundtrip.
    #[test]
    fn block_hash_bincode_roundtrip(bytes in prop::array::uniform32(0u8..)) {
        let hash = BlockHash::new(bytes);
        let encoded = bincode::serialize(&hash).unwrap();
        let decoded: BlockHash = bincode::deserialize(&encoded).unwrap();
        prop_assert_eq!(decoded.as_bytes(), hash.as_bytes());
    }

    /// TxHash bincode serialization roundtrip.
    #[test]
    fn tx_hash_bincode_roundtrip(bytes in prop::array::uniform32(0u8..)) {
        let hash = TxHash::new(bytes);
        let encoded = bincode::serialize(&hash).unwrap();
        let decoded: TxHash = bincode::deserialize(&encoded).unwrap();
        prop_assert_eq!(decoded.as_bytes(), hash.as_bytes());
    }

    /// Timestamp ordering: new(a) <= new(b) iff a <= b.
    #[test]
    fn timestamp_ordering(a in 0u64..u64::MAX, b in 0u64..u64::MAX) {
        let ta = Timestamp::new(a);
        let tb = Timestamp::new(b);
        prop_assert_eq!(ta <= tb, a <= b);
        prop_assert_eq!(ta == tb, a == b);
    }

    /// Timestamp elapsed_since: elapsed_since(now) = now - self (saturating).
    #[test]
    fn timestamp_elapsed_since(base in 0u64..1_000_000, offset in 0u64..1_000_000) {
        let t = Timestamp::new(base);
        let now = Timestamp::new(base + offset);
        prop_assert_eq!(t.elapsed_since(now), offset);
    }

    /// Timestamp elapsed_since saturates to 0 when now < self.
    #[test]
    fn timestamp_elapsed_since_saturates(
        base in 1u64..1_000_000,
        deficit in 1u64..1_000_000,
    ) {
        let later = Timestamp::new(base + deficit);
        let earlier = Timestamp::new(base);
        prop_assert_eq!(later.elapsed_since(earlier), 0);
    }

    /// Timestamp has_expired agrees with manual arithmetic.
    #[test]
    fn timestamp_has_expired_correct(
        start in 0u64..500_000,
        duration in 1u64..500_000,
        offset in 0u64..1_000_000,
    ) {
        let t = Timestamp::new(start);
        let now = Timestamp::new(start.saturating_add(offset));
        prop_assert_eq!(t.has_expired(duration, now), offset >= duration);
    }

    /// BrnAmount: from_brn and to_brn are inverses for whole units.
    #[test]
    fn brn_amount_unit_roundtrip(units in 0u128..1_000_000_000) {
        let amount = BrnAmount::from_brn(units);
        prop_assert_eq!(amount.to_brn(), units);
    }

    /// BrnAmount: raw roundtrip.
    #[test]
    fn brn_amount_raw_roundtrip(raw in 0u128..u128::MAX / 2) {
        let amount = BrnAmount::new(raw);
        prop_assert_eq!(amount.raw(), raw);
    }

    /// BrnAmount: checked_add(a, b) == Some(a + b) when no overflow.
    #[test]
    fn brn_amount_checked_add(a in 0u128..u128::MAX / 2, b in 0u128..u128::MAX / 2) {
        let sum = BrnAmount::new(a).checked_add(BrnAmount::new(b));
        prop_assert_eq!(sum, Some(BrnAmount::new(a + b)));
    }

    /// BrnAmount: checked_sub returns None when b > a.
    #[test]
    fn brn_amount_checked_sub_underflow(a in 0u128..1_000_000, b in 0u128..1_000_000) {
        let result = BrnAmount::new(a).checked_sub(BrnAmount::new(b));
        if b > a {
            prop_assert!(result.is_none());
        } else {
            prop_assert_eq!(result, Some(BrnAmount::new(a - b)));
        }
    }

    /// BrnAmount: saturating_sub never panics and returns ZERO on underflow.
    #[test]
    fn brn_amount_saturating_sub(a in 0u128..1_000_000, b in 0u128..1_000_000) {
        let result = BrnAmount::new(a).saturating_sub(BrnAmount::new(b));
        if b > a {
            prop_assert_eq!(result, BrnAmount::ZERO);
        } else {
            prop_assert_eq!(result, BrnAmount::new(a - b));
        }
    }

    /// TrstAmount: from_trst and to_trst roundtrip.
    #[test]
    fn trst_amount_unit_roundtrip(units in 0u128..1_000_000_000) {
        let amount = TrstAmount::from_trst(units);
        prop_assert_eq!(amount.to_trst(), units);
    }

    /// BrnAmount: is_zero matches raw == 0.
    #[test]
    fn brn_amount_is_zero(raw in 0u128..1_000) {
        let amount = BrnAmount::new(raw);
        prop_assert_eq!(amount.is_zero(), raw == 0);
    }
}
