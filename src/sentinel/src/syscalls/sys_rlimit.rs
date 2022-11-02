use limit::{is_valid_resource, Context as LimitContext, Limit};
use mem::Addr;
use platform::Context as PlatformContext;
use utils::{bail_libc, SysError, SysResult};

use crate::context;

// prlimit64 implements linux syscall prlimit64(2)
pub fn prlimit64(regs: &libc::user_regs_struct) -> super::Result {
    let tid = regs.rdi as i32;
    let resource = regs.rsi as u32;
    if !is_valid_resource(resource) {
        bail_libc!(libc::EINVAL);
    }
    let new_rlim_addr = Addr(regs.rdx);
    let old_rlim_addr = Addr(regs.r10);

    let ctx = context::context();
    let task = ctx.task();
    let new_lim = if new_rlim_addr.0 == 0 {
        None
    } else {
        let mut b = [0; std::mem::size_of::<libc::rlimit64>()];
        if task.copy_in_bytes(new_rlim_addr, &mut b).is_err() {
            bail_libc!(libc::EFAULT);
        }
        let rlimit = unsafe { std::ptr::read(b.as_ptr() as *const libc::rlimit64) };
        Some(Limit::from_libc_rlimit64(&rlimit))
    };

    if tid < 0 {
        bail_libc!(libc::EINVAL);
    }

    // This works because we only target single threaded application.
    if tid > 0 && ctx.tid().as_raw() != tid {
        bail_libc!(libc::ESRCH);
    }

    let old_lim = prlimit64_impl(resource, new_lim)?;

    if old_rlim_addr.0 != 0 {
        let old_lim = libc::rlimit64 {
            rlim_cur: old_lim.cur,
            rlim_max: old_lim.max,
        };
        let b = unsafe {
            std::slice::from_raw_parts(
                &old_lim as *const _ as *const u8,
                std::mem::size_of::<libc::rlimit64>(),
            )
        };
        task.copy_out_bytes(old_rlim_addr, b)
            .map_err(|_| SysError::new(libc::EFAULT))?;
    }
    Ok(0)
}

fn prlimit64_impl(resource: u32, new_lim: Option<Limit>) -> SysResult<Limit> {
    match new_lim {
        None => {
            let ctx = context::context();
            let limits = ctx.limits();
            Ok(limits.get_resource(resource))
        }
        Some(new_lim) => {
            if !is_setable_resource(resource) {
                bail_libc!(libc::EPERM);
            }
            let ctx = context::context();
            let privileged = true;
            let mut lim = ctx.limits_mut();
            Ok(lim.set_resource(resource, new_lim, privileged)?)
        }
    }
}

fn is_setable_resource(resource: u32) -> bool {
    matches!(
        resource,
        libc::RLIMIT_NOFILE
            | libc::RLIMIT_AS
            | libc::RLIMIT_CPU
            | libc::RLIMIT_DATA
            | libc::RLIMIT_FSIZE
            | libc::RLIMIT_MEMLOCK
            | libc::RLIMIT_STACK
            | libc::RLIMIT_CORE
            | libc::RLIMIT_RSS
            | libc::RLIMIT_NPROC
    )
}
