use std::{cell::RefCell, rc::Rc};

use fs::{attr::stat_from_attrs, DirentRef, File};
use mem::Addr;
use utils::{bail_libc, SysError, SysResult};

use crate::context;

use super::sys_file::{copy_in_path, file_op_on};

// stat implements linux syscall stat(2)
pub fn stat(regs: &libc::user_regs_struct) -> super::Result {
    let addr = Addr(regs.rdi);
    let stat_addr = Addr(regs.rsi);

    let (path, is_dir) = copy_in_path(addr, false)?;
    file_op_on(libc::AT_FDCWD, &path, true, |_, d, _| {
        stat_impl(d, is_dir, stat_addr)
    })
    .map(|()| 0)
}

fn stat_impl(d: &DirentRef, is_dir: bool, stat_addr: Addr) -> SysResult<()> {
    let d_ref = d.borrow();
    let sattr = d_ref.stable_attr();
    if is_dir && !sattr.is_directory() {
        bail_libc!(libc::ENOTDIR);
    }
    let uattr = d_ref.unstable_attr()?;
    let ctx = &*context::context();
    let task = ctx.task();
    let s = stat_from_attrs(sattr, uattr, ctx);
    let b = unsafe {
        std::slice::from_raw_parts(
            &s as *const _ as *const u8,
            std::mem::size_of::<libc::stat>(),
        )
    };
    task.copy_out_bytes(stat_addr, b).map(|_| ())
}

// fstat implements linux syscall fstat(2)
pub fn fstat(regs: &libc::user_regs_struct) -> super::Result {
    let fd = regs.rdi as i32;
    let stat_addr = Addr(regs.rsi);
    let file = {
        let ctx = context::context();
        let mut task = ctx.task_mut();
        task.get_file(fd).ok_or_else(|| SysError::new(libc::EBADF))
    }?;
    fstat_impl(&file, stat_addr).map(|()| 0)
}

fn fstat_impl(f: &Rc<RefCell<File>>, stat_addr: Addr) -> SysResult<()> {
    let uattr = f.borrow().unstable_attr()?;
    let sattr = {
        let dirent = f.borrow().dirent();
        let dirent = dirent.borrow();
        dirent.inode().stable_attr()
    };
    let ctx = &*context::context();
    let stat = stat_from_attrs(sattr, uattr, ctx);
    let bytes = unsafe {
        std::slice::from_raw_parts(
            &stat as *const _ as *const u8,
            std::mem::size_of::<libc::stat>(),
        )
    };
    let task = ctx.task();
    task.copy_out_bytes(stat_addr, bytes)?;
    Ok(())
}

// fstatat implements linux syscall newfstatat(2)
pub fn fstatat(regs: &libc::user_regs_struct) -> super::Result {
    let dirfd = regs.rdi as i32;
    let path_addr = Addr(regs.rsi);
    let stat_buf = Addr(regs.rdx);
    let flags = regs.r10 as i32;

    let (path, is_dir) = copy_in_path(path_addr, flags & libc::AT_EMPTY_PATH != 0)?;

    match path.as_str() {
        "" => {
            let file = {
                let ctx = context::context();
                let mut task = ctx.task_mut();
                task.get_file(dirfd)
                    .ok_or_else(|| SysError::new(libc::EBADF))
            }?;
            fstat_impl(&file, stat_buf).map(|()| 0)
        }
        path => {
            let resolve = is_dir || flags & libc::AT_SYMLINK_NOFOLLOW == 0;
            file_op_on(dirfd, path, resolve, |_, d, _| {
                stat_impl(d, is_dir, stat_buf)
            })
            .map(|()| 0)
        }
    }
}

// lstat implements linux syscall lstat(2)
pub fn lstat(regs: &libc::user_regs_struct) -> super::Result {
    let addr = Addr(regs.rdi);
    let stat_addr = Addr(regs.rsi);

    let (path, is_dir) = copy_in_path(addr, false)?;
    file_op_on(libc::AT_FDCWD, &path, is_dir, |_, d, _| {
        stat_impl(d, is_dir, stat_addr)
    })
    .map(|()| 0)
}
