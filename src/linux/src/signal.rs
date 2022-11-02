#[derive(Debug, Copy, Clone, PartialOrd, PartialEq, Ord, Eq, Hash)]
pub struct Signal(pub i32);

impl Signal {
    pub fn is_valid(&self) -> bool {
        *self > Self(0) && *self <= Self::max()
    }

    pub const fn max() -> Self {
        Self(64)
    }

    pub const fn unblocked() -> Self {
        Self(libc::SIGKILL | libc::SIGSTOP)
    }
}

pub const SIGNAL_SET_SIZE: i32 = 8;

pub type SignalSet = u64;

#[derive(Copy, Clone, Debug, Default)]
#[repr(C)]
pub struct SigAction {
    pub handler: u64,
    pub flags: u64,
    pub restorer: u64,
    pub mask: SignalSet,
}

pub const SIG_ACTION_SIZE: usize = std::mem::size_of::<SigAction>();
