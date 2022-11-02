use mem::Addr;
use platform::Context;
use utils::{bail_libc, SysError};

use crate::{context, kernel::task::ExitStatus};

// exit implements linux syscall exit(2)
pub fn exit(regs: &libc::user_regs_struct) -> super::Result {
    let code = regs.rdi as i32;
    let ctx = context::context();
    ctx.task_mut()
        .set_exit_status(ExitStatus { code, sig_no: 0 });
    Ok(0)
}

// exit_group implements linux syscall exit_group(2)
pub fn exit_group(regs: &libc::user_regs_struct) -> super::Result {
    let status = regs.rdi as i32;
    let ctx = context::context();
    ctx.task_mut().prepare_group_exit(ExitStatus {
        code: status,
        sig_no: 0,
    });
    Ok(0)
}

// set_tid_address implements linux syscall set_tid_address(2)
pub fn set_tid_address(regs: &libc::user_regs_struct) -> super::Result {
    let tid = Addr(regs.rdi);
    let ctx = context::context();
    let mut task = ctx.task_mut();
    task.set_clear_tid(tid);
    Ok(ctx.tid().as_raw() as usize)
}

// getpid implements linux syscall getpid(2)
// FIXME: Just setting it to ctx.tid()
pub fn getpid(_regs: &libc::user_regs_struct) -> super::Result {
    let ctx = context::context();
    Ok(ctx.tid().as_raw() as usize)
}

// gettid implements linux syscall gettid(2)
// FIXME: Just setting it to ctx.tid()
pub fn gettid(_regs: &libc::user_regs_struct) -> super::Result {
    let ctx = context::context();
    Ok(ctx.tid().as_raw() as usize)
}

// sched_getaffinity implements linux syscall sched_getaffinity(2)
pub fn sched_getaffinity(regs: &libc::user_regs_struct) -> super::Result {
    let pid = regs.rdi as i32;
    let cpusetsize = regs.rsi as usize;
    let mask_addr = Addr(regs.rdx);

    if cpusetsize % 8 != 0 {
        bail_libc!(libc::EINVAL);
    }

    let ctx = context::context();

    // We only target single-threaded application, so if the provided pid does not match current
    // pid, return ESRCH.
    if pid != ctx.tid().as_raw() {
        bail_libc!(libc::ESRCH);
    }

    let task = ctx.task();
    let mask = task.cpu_mask();
    if cpusetsize < mask.len() {
        bail_libc!(libc::EINVAL);
    }
    task.copy_out_bytes(mask_addr, mask).map(|_| mask.len())
}
