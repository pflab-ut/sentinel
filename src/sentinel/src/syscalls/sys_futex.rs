use mem::Addr;
use utils::{bail_libc, err_libc, SysError};

use crate::context;

// set_robust_list implements linux syscall set_robust_list(2)
pub fn set_robust_list(regs: &libc::user_regs_struct) -> super::Result {
    let head = Addr(regs.rdi);
    let length = regs.rsi;
    if length as usize != std::mem::size_of::<linux::RobustListHead>() {
        err_libc!(libc::EINVAL)
    } else {
        let ctx = context::context();
        let mut task = ctx.task_mut();
        task.set_robust_list(head);
        Ok(0)
    }
}

// futex implements linux syscall futex(2)
// FIXME: This is syscall is basically ignored at this point.
pub fn futex(regs: &libc::user_regs_struct) -> super::Result {
    // let addr = Addr(regs.rdi);
    let futex_op = regs.rsi as i32;
    // let val = regs.rdx;
    // let nreq = regs.r10 as i32;
    // let timeout = regs.r10 as usize;
    // let naddr = regs.r8 as usize;
    let val3 = regs.r9 as i32;

    let cmd = futex_op & !(linux::FUTEX_PRIVATE_FLAG | linux::FUTEX_CLOCK_REALTIME);
    // let private = (futex_op & linux::FUTEX_PRIVATE_FLAG) != 0;
    // let clock_realtime = (futex_op & linux::FUTEX_PRIVATE_FLAG) == linux::FUTEX_CLOCK_REALTIME;
    let mask = val3 as u32;

    match cmd {
        linux::FUTEX_WAKE | linux::FUTEX_WAKE_BITSET => {
            let mask = if cmd == linux::FUTEX_WAKE {
                !(0u32)
            } else {
                mask
            };
            if mask == 0 {
                bail_libc!(libc::EINVAL);
            }
            // let val = if val <= 0 { 1 } else { val };
            Ok(0)
        }
        _ => unimplemented!(),
    }
}
