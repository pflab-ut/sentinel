#[derive(PartialEq, Eq, Copy, Clone, Default, Hash)]
pub struct Range<T> {
    pub start: T,
    pub end: T,
}

impl<T: std::fmt::Debug> std::fmt::Debug for Range<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{:#x?}, {:#x?})", self.start, self.end)
    }
}

impl<T: num::Integer> Ord for Range<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.start.cmp(&other.start)
    }
}

impl<T: num::Integer> PartialOrd for Range<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T: num::Integer + Copy> Range<T> {
    #[inline]
    pub fn len(&self) -> T {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    pub fn intersect(&self, r: &Self) -> Self {
        let start = std::cmp::max(r.start, self.start);
        let end = std::cmp::max(std::cmp::min(r.end, self.end), start);
        Self { start, end }
    }

    #[inline]
    pub fn overlaps(&self, r: &Self) -> bool {
        self.start < r.end && r.start < self.end
    }

    #[inline]
    pub fn can_split_at(&self, k: T) -> bool {
        self.start < k && k < self.end
    }

    #[inline]
    pub fn contains(&self, k: T) -> bool {
        self.start <= k && k < self.end
    }

    #[inline]
    pub fn is_superset_of(&self, r: &Self) -> bool {
        self.start <= r.start && r.end <= self.end
    }

    #[inline]
    pub fn is_well_formed(&self) -> bool {
        self.start <= self.end
    }
}

pub type FileRange = Range<u64>;
