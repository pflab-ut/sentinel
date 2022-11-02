use fs::{Context, File};
use mem::{Addr, IoOpts, IoSequence};
use std::{cell::RefCell, rc::Rc};
use utils::{bail_libc, SysError, SysErrorKind, SysResult};

use crate::context;

// read implements linux syscall read(2)
pub fn read(regs: &libc::user_regs_struct) -> super::Result {
    let fd = regs.rdi as i32;
    let buf = Addr(regs.rsi);
    let count = regs.rdx;

    let ctx = &*context::context();
    let mut task = ctx.task_mut();
    let file = task
        .get_file(fd)
        .ok_or_else(|| SysError::new(libc::EBADF))?;
    if !file.borrow().flags().read {
        bail_libc!(libc::EBADF);
    }
    let count = count as i32;
    if count < 0 {
        bail_libc!(libc::EINVAL);
    }
    let mut dst = task.single_io_sequence(buf, count, IoOpts::default())?;
    match readv(&file, &mut dst, ctx) {
        Ok(n) => Ok(n),
        Err(err) if err.code() == libc::EOF => Ok(0),
        Err(err) => Err(err),
    }
}

fn readv(file: &Rc<RefCell<File>>, dst: &mut IoSequence, ctx: &dyn Context) -> SysResult<usize> {
    match file.borrow_mut().readv(dst, ctx) {
        Ok(n) => Ok(n),
        Err(err) if err.kind() == SysErrorKind::ErrWouldBlock => todo!(),
        Err(err) => Err(err),
    }
}

// pread64 implements linux syscall pread64(2)
pub fn pread64(regs: &libc::user_regs_struct) -> super::Result {
    let fd = regs.rdi as i32;
    let addr = Addr(regs.rsi);
    let size = regs.rdx as u32;
    let offset = regs.r10 as i64;

    let ctx = &*context::context();
    let mut task = ctx.task_mut();
    let file = task
        .get_file(fd)
        .ok_or_else(|| SysError::new(libc::EBADF))?;
    if offset < 0 || offset.checked_add(size as i64).is_none() {
        bail_libc!(libc::EINVAL);
    }
    if !file.borrow().flags().pread {
        bail_libc!(libc::ESPIPE);
    }
    if !file.borrow().flags().read {
        bail_libc!(libc::EBADF);
    }
    let size = size as i32;
    if size < 0 {
        bail_libc!(libc::EINVAL);
    }
    let mut dst = task.single_io_sequence(addr, size, IoOpts::default())?;
    preadv(&file, &mut dst, offset, ctx)
}

fn preadv(
    file: &Rc<RefCell<File>>,
    dst: &mut IoSequence,
    offset: i64,
    ctx: &dyn Context,
) -> SysResult<usize> {
    match file.borrow().preadv(dst, offset, ctx) {
        Ok(n) => Ok(n),
        Err(err) if err.kind() == SysErrorKind::ErrWouldBlock => todo!(),
        Err(err) => Err(err),
    }
}
