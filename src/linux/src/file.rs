pub const F_SEAL_SEAL: i32 = 0x0001;
pub const F_SEAL_SHRINK: i32 = 0x0002;
pub const F_SEAL_GROW: i32 = 0x0004;
pub const F_SEAL_WRITE: i32 = 0x0008;

pub const MAX_SYMLINK_TRAVERSALS: u32 = 40;

pub const MODE_OTHER_READ: u16 = 0o4;
pub const MODE_OTHER_WRITE: u16 = 0o2;
pub const MODE_OTHER_EXEC: u16 = 0o1;
pub const PERMISSION_MASK: u16 = 0o777;

#[derive(Clone, Copy, Debug)]
pub struct FileMode(pub u16);

impl std::ops::Shr<usize> for FileMode {
    type Output = Self;
    fn shr(self, rhs: usize) -> Self::Output {
        Self(self.0 >> rhs)
    }
}

impl std::ops::BitAnd for FileMode {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

impl std::ops::Not for FileMode {
    type Output = Self;
    fn not(self) -> Self::Output {
        Self(!self.0)
    }
}

impl FileMode {
    pub fn permissions(&self) -> FileMode {
        Self(self.0 & PERMISSION_MASK)
    }
}
