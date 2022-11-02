use std::{
    os::unix::prelude::OsStrExt,
    path::{Path, PathBuf},
    rc::Rc,
};

use fs::{attr::PermMask, Context, DirentRef};
use mem::Addr;
use utils::{bail_libc, SysError};

use crate::context;

use super::sys_file::{copy_in_path, file_op_on};

// getcwd implements linux syscall getcwd(2)
pub fn getcwd(regs: &libc::user_regs_struct) -> super::Result {
    let addr = Addr(regs.rdi);
    let size = regs.rsi as u32;

    let ctx = context::context();
    let root = ctx.root_directory();
    let wd = ctx.working_directory();

    let mut bytes = pathname_for_cwd(root, wd).as_os_str().as_bytes().to_vec();
    bytes.push(0);

    if bytes.len() > size as usize {
        bail_libc!(libc::ERANGE);
    }
    let task = ctx.task();
    task.copy_out_bytes(addr, &bytes)
}

// FIXME: naive implementation...
fn pathname_for_cwd(root: &DirentRef, wd: &DirentRef) -> PathBuf {
    let mut cur = wd.clone();
    let mut path = PathBuf::new();
    while Rc::as_ptr(&cur) != Rc::as_ptr(root) {
        let parent = {
            let cur_ref = cur.borrow();
            let p = Path::new(cur_ref.name());
            path = p.join(&path);
            cur_ref.parent().upgrade().unwrap()
        };
        cur = parent;
    }
    let p = Path::new("/");
    p.join(&path)
}

// chdir implements linux syscall chdir(2)
pub fn chdir(regs: &libc::user_regs_struct) -> super::Result {
    let addr = Addr(regs.rdi);
    let (path, is_dir) = copy_in_path(addr, false)?;
    if !is_dir {
        bail_libc!(libc::ENOTDIR);
    }

    file_op_on(libc::AT_FDCWD, &path, true, |_, dir, _| {
        let d = dir.borrow();
        let inode = d.inode();
        if !inode.stable_attr().is_directory() {
            bail_libc!(libc::ENOTDIR);
        }
        let ctx = context::context();
        inode.check_permission(
            PermMask {
                read: false,
                write: false,
                execute: true,
            },
            &*ctx,
        )?;
        drop(d);
        drop(ctx);
        let mut ctx = context::context_mut();
        ctx.set_working_directory(dir.clone());
        Ok(())
    })?;
    Ok(0)
}
