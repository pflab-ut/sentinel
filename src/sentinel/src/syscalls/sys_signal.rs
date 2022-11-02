use mem::Addr;
use platform::Context;
use utils::{bail_libc, SysError};

use crate::context;

// sigaltstack implements linux syscall sigaltstack(2)
pub fn sigaltstack(args: &libc::user_regs_struct) -> super::Result {
    let set_addr = args.rdi as u64;
    let old_addr = args.rsi as u64;
    let ctx = context::context();
    let task = ctx.task();
    let alt = task.signal_stack();
    if old_addr != 0 {
        task.copy_out_signal_stack(Addr(old_addr), &alt)?;
    }
    if set_addr != 0 {
        match task.copy_in_signal_stack(Addr(set_addr)) {
            Ok(alt) => {
                drop(task);
                if !ctx.task_mut().set_signal_stack(alt) {
                    bail_libc!(libc::EPERM);
                }
            }
            Err(err) => return Err(err),
        }
    }
    Ok(0)
}

// rt_sigaction implements linux syscall rt_sigaction(2)
pub fn rt_sigaction(args: &libc::user_regs_struct) -> super::Result {
    let signum = args.rdi as i32;
    let new_act = Addr(args.rsi);
    let old_act = Addr(args.rdx);
    let sigset_size = args.r10 as i32;

    if sigset_size != linux::SIGNAL_SET_SIZE {
        bail_libc!(libc::EINVAL);
    }

    let ctx = context::context();
    let new_act = if new_act.0 == 0 {
        None
    } else {
        let task = ctx.task();
        let mut buf = [0; linux::SIG_ACTION_SIZE];
        task.copy_in_bytes(new_act, &mut buf)?;
        let act: linux::SigAction = unsafe { std::ptr::read(buf.as_ptr() as *const _) };
        Some(act)
    };
    let mut task = ctx.task_mut();
    let act = task.set_sigaction(linux::Signal(signum), new_act)?;
    if old_act.0 != 0 {
        let src = unsafe {
            std::slice::from_raw_parts(&act as *const _ as *const u8, linux::SIG_ACTION_SIZE)
        };
        task.copy_out_bytes(old_act, src)?;
    }
    Ok(0)
}

// rt_sigprocmask implements linux syscall rt_sigprocmask(2)
pub fn rt_sigprocmask(args: &libc::user_regs_struct) -> super::Result {
    let how = args.rdi as i32;
    let set_addr = Addr(args.rsi);
    let old_addr = Addr(args.rdx);
    let sigset_size = args.r10 as i32;

    if sigset_size != linux::SIGNAL_SET_SIZE {
        bail_libc!(libc::EINVAL);
    }
    let ctx = context::context();
    let task = ctx.task();
    let old_mask = task.signal_mask();
    if set_addr.0 != 0 {
        let mask = task.copy_in_sig_set(set_addr, sigset_size)?;
        match how {
            libc::SIG_BLOCK => task.set_signal_mask(mask | old_mask),
            libc::SIG_UNBLOCK => task.set_signal_mask(old_mask & !mask),
            libc::SIG_SETMASK => task.set_signal_mask(mask),
            _ => bail_libc!(libc::EINVAL),
        }
    }
    if old_addr.0 != 0 {
        task.copy_out_sig_set(old_addr, old_mask).map(|()| 0)
    } else {
        Ok(0)
    }
}

// tgkill implements linux syscall tgkill(2)
pub fn tgkill(regs: &libc::user_regs_struct) -> super::Result {
    let tgid = regs.rdi as i32;
    let tid = regs.rsi as i32;
    let _sig = regs.rdx as i32;

    if tgid <= 0 || tid <= 0 {
        bail_libc!(libc::EINVAL);
    }

    let ctx = context::context();
    if ctx.tid().as_raw() != tid {
        bail_libc!(libc::ESRCH);
    }
    // FIXME: properly implement killing
    Ok(0)
}
