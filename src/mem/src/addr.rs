use super::PAGE_SIZE;
use utils::Range;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Addr(pub u64);

impl Addr {
    #[inline]
    pub fn round_up(&self) -> Option<Self> {
        let addr = self.0.checked_add(PAGE_SIZE as u64 - 1)?;
        Some(Addr(addr).round_down())
    }

    #[inline]
    pub fn must_round_up(&self) -> Self {
        self.round_up().expect("must_round_up failed")
    }

    pub const fn round_down(&self) -> Self {
        Addr(self.0 & !(PAGE_SIZE as u64 - 1))
    }

    #[inline]
    pub fn to_range(self, length: u64) -> Option<AddrRange> {
        self.add_length(length).map(|end| AddrRange {
            start: self.0,
            end: end.0,
        })
    }

    #[inline]
    pub fn add_length(&self, length: u64) -> Option<Self> {
        let end = self.0.checked_add(length)?;
        Some(Addr(end))
    }

    #[inline]
    pub fn page_offset(&self) -> u64 {
        self.0 & (PAGE_SIZE as u64 - 1)
    }
}

impl std::ops::Add for Addr {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        Self(self.0 + other.0)
    }
}

impl std::ops::Sub for Addr {
    type Output = Self;
    fn sub(self, other: Self) -> Self {
        Self(self.0 - other.0)
    }
}

impl std::ops::AddAssign for Addr {
    fn add_assign(&mut self, rhs: Self) {
        self.0 = self.0 + rhs.0;
    }
}

impl std::ops::SubAssign for Addr {
    fn sub_assign(&mut self, other: Self) {
        self.0 = self.0 - other.0;
    }
}

impl std::fmt::Display for Addr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

pub type AddrRange = Range<u64>;
