mod context;
mod feature_set;
pub mod signal;
mod stack;

use limit::LimitSet;
use mem::{Addr, PAGE_SIZE};
use rand::Rng;
use utils::{bail_libc, SysError, SysResult};

pub use context::*;
pub use feature_set::*;
pub use stack::*;

#[derive(PartialEq, Eq, Copy, Clone, Debug)]
pub enum MmapDirection {
    MmapBottomUp,
    MmapTopDown,
}

impl Default for MmapDirection {
    fn default() -> Self {
        MmapDirection::MmapBottomUp
    }
}

pub const MAX_ADDR: Addr = Addr((1 << 47) - PAGE_SIZE as u64);
pub const MAX_STACK_RAND: u64 = 16 << 30;
pub const MAX_MMAP_RAND: u64 = (1 << 28) * PAGE_SIZE as u64;
pub const MIN_GAP: u64 = (128 << 20) + MAX_STACK_RAND;

const PREFERRED_TOP_DOWN_ALLOC_MIN: u64 = 0x7e8000000000;
const PREFERRED_ALLOCATION_GAP: u64 = 128 << 30;
pub const PREFERRED_TOP_DOWN_BASE_MIN: Addr =
    Addr(PREFERRED_TOP_DOWN_ALLOC_MIN + PREFERRED_ALLOCATION_GAP);
pub const PREFERRED_PIE_LOAD_ADDR: Addr = Addr(MAX_ADDR.0 / 3 * 2);

pub const MIN_MMAP_RAND: u64 = (1 << 26) * PAGE_SIZE as u64;

pub static CPUID_INSTRUCTION: &[u8] = &[0xf, 0xa2];

#[derive(Default, Copy, Clone, Debug)]
pub struct MmapLayout {
    pub min_addr: Addr,
    pub max_addr: Addr,
    pub bottom_up_base: Addr,
    pub top_down_base: Addr,
    pub default_direction: MmapDirection,
    pub max_stack_rand: u64,
}

impl MmapLayout {
    pub fn new(min: Addr, max: Addr, limits: &LimitSet) -> SysResult<MmapLayout> {
        let min = min.round_up().ok_or_else(|| SysError::new(libc::EINVAL))?;
        let max = std::cmp::min(max, MAX_ADDR).round_down();
        if min > max {
            bail_libc!(libc::EINVAL);
        }
        let stack_size = limits.get_stack();
        let gap = Addr(stack_size.cur);
        let gap = std::cmp::max(gap, Addr(MIN_GAP));
        let gap = std::cmp::min(gap, Addr((max.0 / 6) * 5));
        let default_direction = if stack_size.cur == limit::INFINITY {
            MmapDirection::MmapBottomUp
        } else {
            MmapDirection::MmapTopDown
        };
        let top_down_min = max.0 - gap.0 - MAX_MMAP_RAND;
        let mut max_rand = Addr(MAX_MMAP_RAND);
        if top_down_min < PREFERRED_TOP_DOWN_BASE_MIN.0 {
            let max_adjust = max_rand.0 - MIN_MMAP_RAND;
            let need_adjust = PREFERRED_TOP_DOWN_BASE_MIN.0 - top_down_min;
            if need_adjust <= max_adjust {
                max_rand = Addr(max_rand.0 - need_adjust);
            }
        }

        let rnd = mmap_rand(max_rand.0);
        let layout = MmapLayout {
            min_addr: min,
            max_addr: max,
            bottom_up_base: Addr(max.0 / 3 + rnd.0).round_down(),
            top_down_base: (max - gap - rnd).round_down(),
            default_direction,
            max_stack_rand: max_rand.0,
        };
        logger::info!("topdownbase: {}", (max - gap - rnd).round_down());
        if !layout.is_valid() {
            panic!("invalid MmapLayout: {:?}", layout)
        }
        Ok(layout)
    }

    pub fn pie_load_address(&self) -> Addr {
        let mut base = PREFERRED_PIE_LOAD_ADDR;
        let max = base.add_length(MAX_MMAP_RAND).unwrap();
        if max > self.max_addr {
            base = Addr(self.top_down_base.0 / 3 * 2);
        }
        base + mmap_rand(MAX_MMAP_RAND)
    }

    pub fn new_test(
        min_addr: Addr,
        max_addr: Addr,
        bottom_up_base: Addr,
        top_down_base: Addr,
    ) -> Self {
        Self {
            min_addr,
            max_addr,
            bottom_up_base,
            top_down_base,
            ..Self::default()
        }
    }

    pub fn is_valid(&self) -> bool {
        if self.min_addr > self.max_addr {
            false
        } else if self.bottom_up_base < self.min_addr {
            false
        } else if self.bottom_up_base > self.max_addr {
            false
        } else if self.top_down_base < self.min_addr {
            false
        } else {
            self.top_down_base <= self.max_addr
        }
    }
}

fn mmap_rand(max: u64) -> Addr {
    let mut rng = rand::thread_rng();
    Addr(rng.gen_range(0..max)).round_down()
}
