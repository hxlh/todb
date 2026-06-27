use std::fmt;
use std::ops::{Add, Range, Sub};

/// Dense monotonic `u64` log sequence number. Required by Raft; never decreases across `append`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Lsn(pub u64);

impl Lsn {
    pub const ZERO: Lsn = Lsn(0);

    pub fn get(self) -> u64 {
        self.0
    }

    pub fn checked_add(self, n: u64) -> Option<Lsn> {
        self.0.checked_add(n).map(Lsn)
    }
}

impl fmt::Display for Lsn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Add<u64> for Lsn {
    type Output = Lsn;
    fn add(self, rhs: u64) -> Lsn {
        Lsn(self.0 + rhs)
    }
}

impl Sub<u64> for Lsn {
    type Output = Lsn;
    fn sub(self, rhs: u64) -> Lsn {
        Lsn(self.0 - rhs)
    }
}

pub type LsnRange = Range<Lsn>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_is_numeric() {
        assert!(Lsn(1) < Lsn(2));
        assert_eq!(Lsn(5), Lsn(5));
    }

    #[test]
    fn arithmetic() {
        assert_eq!(Lsn(3) + 4, Lsn(7));
        assert_eq!(Lsn(10) - 4, Lsn(6));
        assert_eq!(Lsn(5).checked_add(1), Some(Lsn(6)));
    }

    #[test]
    fn checked_add_overflow() {
        assert_eq!(Lsn(u64::MAX).checked_add(1), None);
    }
}
