use auth::Context;
use mem::{Addr, PAGE_SIZE};
use utils::{bail_libc, SysError, SysResult};

use crate::context;

// For now, unconditionally report a single NUMA policy
const MAX_NODES: u64 = 1;
const ALLOWED_NODEMASK: u64 = (1 << MAX_NODES) - 1;

// mbind implements linux syscall mbind(2)
pub fn mbind(regs: &libc::user_regs_struct) -> super::Result {
    let addr = Addr(regs.rdi);
    let len = regs.rsi;
    let mode = linux::NumaPolicy(regs.rdx as i32);
    let nodemask = Addr(regs.r10);
    let maxnode = regs.r8 as u32;
    let flags = regs.r9 as u32;

    if flags & !linux::MPOL_MF_VALID as u32 != 0 {
        bail_libc!(libc::EINVAL);
    }

    let ctx = context::context();
    let task = ctx.task();
    if flags & linux::MPOL_MF_MOVE_ALL as u32 != 0
        && !ctx
            .credentials()
            .has_capability(&linux::Capability::cap_sys_nice())
    {
        bail_libc!(libc::EPERM);
    }

    let (mode, nodemask) = copy_in_mempolicy_nodemask(mode, nodemask, maxnode)?;
    task.memory_manager()
        .borrow_mut()
        .set_numa_policy(addr, len, mode, nodemask)?;
    Ok(0)
}

fn copy_in_nodemask(addr: Addr, max_node: u32) -> SysResult<u64> {
    let bits = max_node - 1;
    if bits > (PAGE_SIZE * 8) as u32 {
        bail_libc!(libc::EINVAL);
    }
    if bits == 0 {
        return Ok(0);
    }
    let num = (bits + 63) / 64;
    let mut buf = vec![0; num as usize * 8];
    let ctx = context::context();
    let task = ctx.task();
    task.copy_in_bytes(addr, &mut buf)?;
    let val = u64::from_le_bytes([
        buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
    ]);
    if val & !ALLOWED_NODEMASK != 0 {
        bail_libc!(libc::EINVAL);
    }

    if buf[8..].iter().all(|b| *b == 0) {
        Ok(val)
    } else {
        bail_libc!(libc::EINVAL);
    }
}

fn copy_in_mempolicy_nodemask(
    mode_with_flags: linux::NumaPolicy,
    nodemask: Addr,
    max_node: u32,
) -> SysResult<(linux::NumaPolicy, u64)> {
    let flags = linux::NumaPolicy(mode_with_flags.0 & linux::MPOL_MODE_FLAGS);
    let mode = linux::NumaPolicy(mode_with_flags.0 & !linux::MPOL_MODE_FLAGS);

    if flags.0 == linux::MPOL_MODE_FLAGS {
        // Can't specify both flags at the same time.
        bail_libc!(libc::EINVAL);
    }

    if mode.0 < 0 || mode >= linux::NumaPolicy::max() {
        bail_libc!(libc::EINVAL);
    }

    let nodemask_val = if nodemask.0 != 0 {
        copy_in_nodemask(nodemask, max_node)?
    } else {
        0
    };

    // TODO: additional checks

    Ok((mode | flags, nodemask_val))
}
