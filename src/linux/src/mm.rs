#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct NumaPolicy(pub i32);

impl NumaPolicy {
    pub const fn default() -> Self {
        Self(0)
    }

    pub const fn preferred() -> Self {
        Self(1)
    }

    pub const fn bind() -> Self {
        Self(2)
    }

    pub const fn interleave() -> Self {
        Self(3)
    }

    pub const fn local() -> Self {
        Self(4)
    }

    pub const fn max() -> Self {
        Self(5)
    }
}

impl std::ops::BitOr for NumaPolicy {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

pub const MPOL_F_RELATIVE_NODES: i32 = 1 << 14;
pub const MPOL_F_STATIC_NODES: i32 = 1 << 15;
pub const MPOL_MODE_FLAGS: i32 = MPOL_F_STATIC_NODES | MPOL_F_RELATIVE_NODES;

pub const MPOL_MF_STRICT: i32 = 1 << 0;
pub const MPOL_MF_MOVE: i32 = 1 << 1;
pub const MPOL_MF_MOVE_ALL: i32 = 1 << 2;

pub const MPOL_MF_VALID: i32 = MPOL_MF_STRICT | MPOL_MF_MOVE | MPOL_MF_MOVE_ALL;
