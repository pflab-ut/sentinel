use std::{cell::RefCell, rc::Rc};

use limit::Context as LimitContext;
use mem::Addr;
use net::Context as NetContext;
use smoltcp::time::Duration;
use utils::{bail_libc, SysError, SysResult};

use crate::context;

// poll implements linux syscall poll(2)
pub fn poll(regs: &libc::user_regs_struct) -> super::Result {
    let fds_addr = Addr(regs.rdi);
    let nfds = regs.rsi;
    let timeout = regs.rdx as i32;

    let mut pfds = copy_in_poll_fds(fds_addr, nfds)?;
    let n = poll_block(&mut pfds, timeout)?;
    if nfds > 0 {
        copy_out_poll_fds(fds_addr, &pfds)?;
    }
    Ok(n)
}

fn poll_block(pfds: &mut [libc::pollfd], timeout: i32) -> SysResult<usize> {
    let ctx = context::context();
    let files = {
        let mut task = ctx.task_mut();
        pfds.iter()
            .map(|pfd| task.get_file(pfd.fd))
            .collect::<Vec<_>>()
    };
    let n = update_readiness(pfds, &files);
    if n > 0 || timeout == 0 {
        return Ok(n);
    }

    let duration = if timeout > 0 {
        Some(Duration::from_millis(timeout as u64))
    } else {
        None
    };
    ctx.wait(duration);

    Ok(update_readiness(pfds, &files))
}

fn update_readiness(pfds: &mut [libc::pollfd], files: &[Option<Rc<RefCell<fs::File>>>]) -> usize {
    let ctx = context::context();
    ctx.poll_wait(true);
    let mut n = 0;
    for (pfd, file) in pfds.iter_mut().zip(files) {
        if pfd.fd < 0 {
            pfd.revents = 0;
            continue;
        }
        match file {
            Some(file) => {
                let r = file.borrow().readiness(pfd.events as u64, &*ctx);
                pfd.revents = (r as i16) & pfd.events;
            }
            None => pfd.revents = libc::POLLNVAL,
        }
        if pfd.revents != 0 {
            n += 1;
        }
    }
    n
}

const FILE_CAP: u64 = 1024 * 1024;
static POLLFD_SIZE: usize = std::mem::size_of::<libc::pollfd>();

fn copy_in_poll_fds(addr: Addr, nfds: u64) -> SysResult<Vec<libc::pollfd>> {
    let ctx = context::context();
    let limits = ctx.limits();
    if nfds > limits.get_resource_capped(libc::RLIMIT_NOFILE, FILE_CAP) {
        bail_libc!(libc::EINVAL);
    }
    let mut pfds = vec![0; nfds as usize * POLLFD_SIZE];
    let task = ctx.task();
    task.copy_in_bytes(addr, &mut pfds)?;
    Ok(pfds
        .chunks_exact(POLLFD_SIZE)
        .map(|b| unsafe { *(b.as_ptr() as *const libc::pollfd) })
        .collect())
}

fn copy_out_poll_fds(addr: Addr, pfds: &[libc::pollfd]) -> SysResult<usize> {
    let bytes = unsafe {
        let ptr = pfds.as_ptr() as *const u8;
        std::slice::from_raw_parts(ptr, pfds.len() * POLLFD_SIZE)
    };
    let ctx = context::context();
    let task = ctx.task();
    task.copy_out_bytes(addr, bytes)
}
