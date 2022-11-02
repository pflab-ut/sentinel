use mem::Addr;
use utils::{err_libc, SysError};

use crate::context;

// timer_create implements linux syscall timer_create(2)
// FIXME: timer_create currently just fill the third argument with next timer id.
// Should be handled properly future.
pub fn timer_create(regs: &libc::user_regs_struct) -> super::Result {
    let timerid_addr = Addr(regs.rdx);

    let ctx = context::context();
    let mut task = ctx.task_mut();
    let id = task.create_timer();
    task.copy_out_bytes(timerid_addr, &id.to_le_bytes())
        .map(|_| 0)
}

// timer_delete implements linux syscall timer_delete(2)
pub fn timer_delete(regs: &libc::user_regs_struct) -> super::Result {
    let timerid = regs.rdi as i32;
    let ctx = context::context();
    let mut task = ctx.task_mut();
    if task.delete_timer(timerid) {
        Ok(0)
    } else {
        err_libc!(libc::EINVAL)
    }
}
