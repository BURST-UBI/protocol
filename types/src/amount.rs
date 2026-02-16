//! Token amount types for BRN and TRST.
//!
//! Amounts are represented as fixed-point integers (u128) to avoid floating-point errors.
//! The smallest unit is 1 raw. Higher denominations are defined by the protocol.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Add, Sub};

/// BRN amount — the birthright / production potential.
///
/// Internally stored as raw units (u128) for precision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BrnAmount(u128);

impl BrnAmount {
    pub const ZERO: Self = Self(0);

    pub fn new(raw: u128) -> Self {
        Self(raw)
    }

    pub fn raw(&self) -> u128 {
        self.0
    }

    pub fn is_zero(&self) -> bool {
        self.0 == 0
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
        write!(f, "{} BRN", self.0)
    }
}

/// TRST amount — the transferable currency.
///
/// Internally stored as raw units (u128) for precision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TrstAmount(u128);

impl TrstAmount {
    pub const ZERO: Self = Self(0);

    pub fn new(raw: u128) -> Self {
        Self(raw)
    }

    pub fn raw(&self) -> u128 {
        self.0
    }

    pub fn is_zero(&self) -> bool {
        self.0 == 0
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
        write!(f, "{} TRST", self.0)
    }
}
