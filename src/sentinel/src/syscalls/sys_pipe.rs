use std::{cell::RefCell, rc::Rc};

use mem::Addr;
use utils::{bail_libc, SysError, SysResult};

use crate::{
    context,
    kernel::pipe::{PipeRef, DEFAULT_PIPE_SIZE},
};

// pipe implements linux syscall pipe(2)
pub fn pipe(regs: &libc::user_regs_struct) -> super::Result {
    let pipefd_addr = Addr(regs.rdi);
    pipe2_impl(pipefd_addr, 0).map(|()| 0)
}

// pipe2 implements linux syscall pipe2(2)
pub fn pipe2(regs: &libc::user_regs_struct) -> super::Result {
    let fd_addr = Addr(regs.rdi);
    let flags = regs.rsi as i32;
    pipe2_impl(fd_addr, flags).map(|()| 0)
}

fn pipe2_impl(addr: Addr, flags: i32) -> SysResult<()> {
    if flags & !(libc::O_NONBLOCK | libc::O_CLOEXEC) != 0 {
        bail_libc!(libc::EINVAL);
    }
    let (mut r, mut w) = {
        let mut pipe = PipeRef::new(DEFAULT_PIPE_SIZE);
        pipe.connect()
    };
    r.set_flags(fs::FileFlags::from_linux_flags(flags).as_settable());
    w.set_flags(fs::FileFlags::from_linux_flags(flags).as_settable());
    let ctx = context::context();
    let mut task = ctx.task_mut();
    let fds = task.fd_table_mut().new_fds(
        0,
        &[&Rc::new(RefCell::new(r)), &Rc::new(RefCell::new(w))],
        fs::FdFlags {
            close_on_exec: flags & libc::O_CLOEXEC != 0,
        },
    )?;
    debug_assert_eq!(fds.len(), 2);
    let bytes = &[
        fds[0].to_le_bytes().as_slice(),
        fds[1].to_le_bytes().as_slice(),
    ]
    .concat();
    task.copy_out_bytes(addr, bytes)
        .map_err(|e| {
            for fd in fds {
                task.fd_table_mut().remove(fd);
            }
            e
        })
        .map(|_| ())
}
