use fs::File;
use mem::{Addr, IoOpts, IoSequence};
use std::{cell::RefCell, rc::Rc};
use utils::{bail_libc, SysError, SysErrorKind, SysResult};

use crate::context;

// write implements linux syscall write(2)
pub fn write(regs: &libc::user_regs_struct) -> super::Result {
    let fd = regs.rdi;
    let addr = Addr(regs.rsi);
    let size = regs.rdx as i32;

    let ctx = context::context();
    let file = ctx
        .task_mut()
        .get_file(fd as i32)
        .ok_or_else(|| SysError::new(libc::EBADF))?;
    if !file.as_ref().borrow().flags().write {
        bail_libc!(libc::EBADF);
    }
    if size < 0 {
        bail_libc!(libc::EINVAL);
    }

    let mut src = {
        let task = ctx.task();
        task.single_io_sequence(addr, size, IoOpts::default())?
    };
    writev_impl(&file, &mut src)
}

fn writev_impl(file: &Rc<RefCell<File>>, src: &mut IoSequence) -> SysResult<usize> {
    let ctx = &*context::context();
    match file.as_ref().borrow_mut().writev(src, ctx) {
        Ok(n) => Ok(n),
        Err(err) if err.kind() != SysErrorKind::ErrWouldBlock => Err(err),
        Err(_) => todo!(),
    }
}

// writev implements linux syscall writev(2)
pub fn writev(regs: &libc::user_regs_struct) -> super::Result {
    let fd = regs.rdi as i32;
    let addr = Addr(regs.rsi);
    let count = regs.rdx as i32;

    let ctx = context::context();
    let file = {
        let mut task = ctx.task_mut();
        task.get_file(fd)
            .ok_or_else(|| SysError::new(libc::EBADF))?
    };
    if !file.borrow().flags().write {
        bail_libc!(libc::EBADF);
    }
    let task = ctx.task();
    let mut src = task.iovecs_io_sequence(addr, count, IoOpts::default())?;
    writev_impl(&file, &mut src)
}
