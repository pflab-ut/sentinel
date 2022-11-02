use mem::Addr;
use utils::{err_libc, SysError};

use crate::context;

// prctl implements linux syscall prctl(2)
pub fn prctl(regs: &libc::user_regs_struct) -> super::Result {
    let option = regs.rdi as i32;
    let arg2 = regs.rsi as u64;
    let _arg3 = regs.rdx as u64;
    let _arg4 = regs.r10 as u64;
    let _arg5 = regs.r8 as u64;

    match option {
        libc::PR_SET_PDEATHSIG => {
            let signal = linux::Signal(arg2 as i32);
            if signal.0 != 0 && signal.is_valid() {
                err_libc!(libc::EINVAL)
            } else {
                let ctx = context::context();
                let mut task = ctx.task_mut();
                task.set_parent_death_signal(signal);
                Ok(0)
            }
        }
        libc::PR_GET_PDEATHSIG => {
            let ctx = context::context();
            let task = ctx.task();
            task.copy_out_bytes(Addr(arg2), &task.parent_death_signal().0.to_le_bytes())
                .map(|_| 0)
        }
        _ => {
            logger::warn!("argument {} is not implemented in prctl(2)", option);
            Ok(0)
        }
    }
}
