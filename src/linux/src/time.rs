pub const CPUCLOCK_PROF: u64 = 0;
pub const CPUCLOCK_VIRT: u64 = 1;
pub const CPUCLOCK_SCHED: u64 = 2;
pub const CPUCLOCK_MAX: u64 = 3;
pub const CLOCK_MASK: u64 = 3;

pub const CLOCK_REALTIME: u64 = 0;
pub const CLOCK_MONOTONIC: u64 = 1;
pub const CLOCK_PROCESS_CPUTIME_ID: u64 = 2;
pub const CLOCK_THREAD_CPUTIME_ID: u64 = 3;
pub const CLOCK_MONOTONIC_RAW: u64 = 4;
pub const CLOCK_REALTIME_COARSE: u64 = 5;
pub const CLOCK_MONOTONIC_COARSE: u64 = 6;
pub const CLOCK_BOOTTIME: u64 = 7;
