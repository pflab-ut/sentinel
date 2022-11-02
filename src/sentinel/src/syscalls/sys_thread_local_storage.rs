use crate::context;

use arch::MAX_ADDR;

use mem::Addr;
use utils::{bail_libc, err_libc, SysError};

// arch_prctl implements linux syscall arch_prctl(2)
pub fn arch_prctl(regs: &mut libc::user_regs_struct) -> super::Result {
    match regs.rdi as u32 {
        linux::ARCH_GET_FS => {
            let addr = Addr(regs.rsi);
            let ctx = context::context();
            let task = ctx.task();
            let fs_base = task.regs().fs_base;
            task.copy_out_bytes(addr, &fs_base.to_le_bytes()).map(|_| 0)
        }
        linux::ARCH_SET_FS => {
            let fs_base = regs.rsi;
            let is_valid_segment_base = fs_base < (MAX_ADDR.0);
            if !is_valid_segment_base {
                bail_libc!(libc::EPERM);
            } else {
                regs.fs = 0;
                regs.fs_base = fs_base;
                Ok(0)
            }
        }
        linux::ARCH_GET_GS | linux::ARCH_SET_GS => {
            logger::warn!("not implemented yet: {}", file!());
            Ok(0)
        }
        _ => err_libc!(libc::EINVAL),
    }
}
