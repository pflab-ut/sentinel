use std::{cell::RefCell, rc::Rc};

use fs::{FdFlags, SettableFileFlags};
use time::Context;

use crate::{context, kernel::eventfd::new_eventfd};

// eventfd implements linux syscall eventfd
pub fn eventfd(mut regs: libc::user_regs_struct) -> super::Result {
    regs.rsi = 0;
    eventfd2(&regs)
}

// eventfd2 implements linux syscall eventfd2
pub fn eventfd2(regs: &libc::user_regs_struct) -> super::Result {
    let flags = regs.rsi as i32;
    let ctx = context::context();
    let mut event = new_eventfd(&|| ctx.now());
    event.set_flags(SettableFileFlags {
        non_blocking: flags & libc::EFD_NONBLOCK != 0,
        ..SettableFileFlags::default()
    });
    let mut task = ctx.task_mut();
    task.new_fd_from(
        0,
        &Rc::new(RefCell::new(event)),
        FdFlags {
            close_on_exec: flags & libc::EFD_CLOEXEC != 0,
        },
    )
    .map(|fd| fd as usize)
}
