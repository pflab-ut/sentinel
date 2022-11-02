use std::{cell::RefCell, rc::Rc};

use utils::{bail_libc, SysError, SysResult};

use crate::{context, kernel::epoll};

// epoll_create1 implements linux syscall epoll_create1(2)
pub fn epoll_create1(regs: &libc::user_regs_struct) -> super::Result {
    let flags = regs.rdi as i32;
    if flags & !libc::EPOLL_CLOEXEC != 0 {
        bail_libc!(libc::EINVAL);
    }
    create_epoll(flags & libc::EPOLL_CLOEXEC != 0).map(|fd| fd as usize)
}

fn create_epoll(close_on_exec: bool) -> SysResult<i32> {
    let file = epoll::new_event_poll();
    let file = Rc::new(RefCell::new(file));
    let ctx = context::context();
    let mut task = ctx.task_mut();
    task.new_fd_from(0, &file, fs::FdFlags { close_on_exec })
}
