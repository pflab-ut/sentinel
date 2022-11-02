use mem::Addr;
use platform::Context;
use time::{Clock, HostClock};
use utils::{bail_libc, err_libc, SysError, SysResult};

use crate::context;

// clock_gettime implements linux syscall clock_gettime(2)
pub fn clock_gettime(regs: &libc::user_regs_struct) -> super::Result {
    let clock_id = regs.rdi as i32;
    let addr = Addr(regs.rsi);
    let c = get_clock(clock_id)?;
    let ts = c.now().as_libc_timespec();
    let bytes = unsafe {
        let bytes = &ts as *const _;
        std::slice::from_raw_parts(bytes as *const u8, std::mem::size_of_val(&ts))
    };
    let ctx = context::context();
    let task = ctx.task();
    task.copy_out_bytes(addr, bytes).map(|_| 0)
}

// clock_nanosleep implements linux syscall clock_nanosleep(2)
pub fn clock_nanosleep(regs: &libc::user_regs_struct) -> super::Result {
    let clock_id = regs.rdi as i32;
    let flags = regs.rsi as i32;
    let request = Addr(regs.rdx);
    let remain = Addr(regs.r10);

    let request = copy_in_timespec(request)?;

    if !is_timespec_valid(&request) {
        bail_libc!(libc::EINVAL);
    }

    if clock_id > 0 && !matches!(
            clock_id,
            libc::CLOCK_REALTIME
                | libc::CLOCK_MONOTONIC
                | libc::CLOCK_BOOTTIME
                | libc::CLOCK_PROCESS_CPUTIME_ID
        ) {
        bail_libc!(libc::EINVAL);
    }

    let c = get_clock(clock_id)?;
    let start = c.now();

    let request = time::Time::from_unix(request.tv_sec, request.tv_nsec);
    let duration = if flags & libc::TIMER_ABSTIME != 0 {
        request - c.now()
    } else {
        request
    };
    c.sleep(duration);
    let now = c.now();
    if now - start < duration && remain.0 != 0 {
        let remaining = duration - now + start;
        let remaining = remaining.as_libc_timespec();
        copy_out_timespec(remain, &remaining)?;
    }
    Ok(0)
}

// FIXME: naive implementation (return appropriate clock according to the given clock_id)
fn get_clock(clock_id: i32) -> SysResult<HostClock> {
    let ctx = context::context();
    if clock_id < 0 {
        if !is_valid_cpu_clock(clock_id) {
            bail_libc!(libc::EINVAL);
        }

        let pid = !(clock_id >> 3);
        if pid != 0 && pid != ctx.tid().as_raw() {
            logger::warn!(
                "returning EINVAL in get_clock since we only target single threaded application"
            );
            bail_libc!(libc::EINVAL);
        }
        match clock_id as u64 & linux::CLOCK_MASK {
            linux::CPUCLOCK_VIRT => Ok(ctx.real_time_clock()),
            linux::CPUCLOCK_PROF | linux::CPUCLOCK_SCHED => Ok(ctx.real_time_clock()),
            _ => err_libc!(libc::EINVAL),
        }
    } else {
        match clock_id {
            libc::CLOCK_REALTIME | libc::CLOCK_REALTIME_COARSE => Ok(ctx.real_time_clock()),
            libc::CLOCK_MONOTONIC
            | libc::CLOCK_MONOTONIC_COARSE
            | libc::CLOCK_MONOTONIC_RAW
            | libc::CLOCK_BOOTTIME => Ok(ctx.real_time_clock()),
            libc::CLOCK_PROCESS_CPUTIME_ID => Ok(ctx.real_time_clock()),
            libc::CLOCK_THREAD_CPUTIME_ID => Ok(ctx.real_time_clock()),
            _ => err_libc!(libc::EINVAL),
        }
    }
}

fn is_valid_cpu_clock(c: i32) -> bool {
    if c & 7 == 7 {
        false
    } else {
        (c as u64) & linux::CLOCK_MASK < linux::CPUCLOCK_MAX
    }
}

fn copy_in_timespec(addr: Addr) -> SysResult<libc::timespec> {
    let ctx = context::context();
    let task = ctx.task();
    let mut buf = vec![0; 16];
    task.copy_in_bytes(addr, &mut buf)?;
    Ok(libc::timespec {
        tv_sec: u64::from_le_bytes([
            buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
        ]) as i64,
        tv_nsec: u64::from_le_bytes([
            buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
        ]) as i64,
    })
}

fn copy_out_timespec(addr: Addr, ts: &libc::timespec) -> SysResult<usize> {
    let ctx = context::context();
    let task = ctx.task();
    let src = [ts.tv_sec.to_le_bytes(), ts.tv_nsec.to_le_bytes()].concat();
    task.copy_out_bytes(addr, &src)
}

fn is_timespec_valid(ts: &libc::timespec) -> bool {
    ts.tv_sec >= 0 && ts.tv_nsec >= 0 && ts.tv_nsec < 1_000_000_000
}
