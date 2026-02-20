//! Token amount types for BRN and TRST.
//!
//! Amounts are represented as fixed-point integers (u128) to avoid floating-point errors.
//! The smallest unit is 1 raw. Higher denominations:
//!   1 BRN  = 10^18 raw
//!   1 mBRN = 10^15 raw (milliBRN)
//!   (same for TRST / mTRST)

use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Add, Sub};

/// 1 BRN in raw units (10^18).
pub const BRN_UNIT: u128 = 1_000_000_000_000_000_000;
/// 1 mBRN in raw units (10^15).
pub const MBRN_UNIT: u128 = 1_000_000_000_000_000;
/// 1 TRST in raw units (10^18).
pub const TRST_UNIT: u128 = 1_000_000_000_000_000_000;
/// 1 mTRST in raw units (10^15).
pub const MTRST_UNIT: u128 = 1_000_000_000_000_000;

/// BRN amount — the birthright / production potential.
///
/// Internally stored as raw units (u128) for precision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BrnAmount(u128);

impl BrnAmount {
    pub const ZERO: Self = Self(0);

    /// Create from raw units.
    pub fn new(raw: u128) -> Self {
        Self(raw)
    }

    /// Create from whole BRN (e.g., `from_brn(336)` = 336 BRN).
    pub fn from_brn(brn: u128) -> Self {
        Self(brn * BRN_UNIT)
    }

    /// Create from milli-BRN.
    pub fn from_mbrn(mbrn: u128) -> Self {
        Self(mbrn * MBRN_UNIT)
    }

    pub fn raw(&self) -> u128 {
        self.0
    }

    pub fn is_zero(&self) -> bool {
        self.0 == 0
    }

    /// Whole BRN component (truncated).
    pub fn to_brn(&self) -> u128 {
        self.0 / BRN_UNIT
    }

    /// Fractional part in raw units after removing whole BRN.
    pub fn fractional_raw(&self) -> u128 {
        self.0 % BRN_UNIT
    }

    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }

    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Self)
    }

    pub fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }
}

impl Add for BrnAmount {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl Sub for BrnAmount {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self(self.0 - rhs.0)
    }
}

impl fmt::Display for BrnAmount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let whole = self.0 / BRN_UNIT;
        let frac = self.0 % BRN_UNIT;
        if frac == 0 {
            write!(f, "{} BRN", whole)
        } else {
            write!(f, "{}.{:018} BRN", whole, frac)
        }
    }
}

/// TRST amount — the transferable currency.
///
/// Internally stored as raw units (u128) for precision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TrstAmount(u128);

impl TrstAmount {
    pub const ZERO: Self = Self(0);

    /// Create from raw units.
    pub fn new(raw: u128) -> Self {
        Self(raw)
    }

    /// Create from whole TRST.
    pub fn from_trst(trst: u128) -> Self {
        Self(trst * TRST_UNIT)
    }

    /// Create from milli-TRST.
    pub fn from_mtrst(mtrst: u128) -> Self {
        Self(mtrst * MTRST_UNIT)
    }

    pub fn raw(&self) -> u128 {
        self.0
    }

    pub fn is_zero(&self) -> bool {
        self.0 == 0
    }

    /// Whole TRST component (truncated).
    pub fn to_trst(&self) -> u128 {
        self.0 / TRST_UNIT
    }

    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }

    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Self)
    }

    pub fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }
}

impl Add for TrstAmount {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl Sub for TrstAmount {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self(self.0 - rhs.0)
    }
}

impl fmt::Display for TrstAmount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let whole = self.0 / TRST_UNIT;
        let frac = self.0 % TRST_UNIT;
        if frac == 0 {
            write!(f, "{} TRST", whole)
        } else {
            write!(f, "{}.{:018} TRST", whole, frac)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brn_unit_conversions() {
        assert_eq!(BrnAmount::from_brn(1).raw(), BRN_UNIT);
        assert_eq!(BrnAmount::from_brn(336).raw(), 336 * BRN_UNIT);
        assert_eq!(BrnAmount::from_mbrn(1000).raw(), BRN_UNIT);
        assert_eq!(BrnAmount::from_brn(1).to_brn(), 1);
    }

    #[test]
    fn trst_unit_conversions() {
        assert_eq!(TrstAmount::from_trst(1).raw(), TRST_UNIT);
        assert_eq!(TrstAmount::from_mtrst(1000).raw(), TRST_UNIT);
    }

    #[test]
    fn display_whole_units() {
        assert_eq!(format!("{}", BrnAmount::from_brn(42)), "42 BRN");
        assert_eq!(format!("{}", TrstAmount::from_trst(100)), "100 TRST");
    }

    #[test]
    fn display_fractional() {
        let half = BrnAmount::new(BRN_UNIT / 2);
        let s = format!("{}", half);
        assert!(s.contains("BRN"));
        assert!(s.starts_with("0."));
    }

    #[test]
    fn arithmetic() {
        let a = BrnAmount::from_brn(10);
        let b = BrnAmount::from_brn(3);
        assert_eq!((a + b).to_brn(), 13);
        assert_eq!((a - b).to_brn(), 7);
        assert_eq!(a.checked_sub(BrnAmount::from_brn(20)), None);
        assert_eq!(a.saturating_sub(BrnAmount::from_brn(20)), BrnAmount::ZERO);
    }
}
