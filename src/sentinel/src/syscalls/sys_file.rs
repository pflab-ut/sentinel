use std::{cell::RefCell, path::Component, rc::Rc};

use auth::{capability_set::CapabilitySet, id::Uid, Context as AuthContext};
use fs::{
    attr::{FilePermissions, PermMask},
    Context, DirentRef, FdFlags, FileFlags,
};
use mem::Addr;

use crate::context;

use utils::{bail_libc, SysError, SysErrorKind, SysResult};

// open implements linux syscall open(2)
pub fn open(regs: &libc::user_regs_struct) -> super::Result {
    let addr = regs.rdi as usize;
    let flags = regs.rsi as u32;
    if flags as i32 & libc::O_CREAT != 0 {
        let mode = linux::FileMode(regs.rdx as u16);
        create_at(libc::AT_FDCWD, Addr(addr as u64), flags, mode)
    } else {
        open_at(libc::AT_FDCWD, Addr(addr as u64), flags)
    }
}

// openat implements linux syscall openat(2)
pub fn openat(regs: &libc::user_regs_struct) -> super::Result {
    let dir_fd = regs.rdi as i32;
    let addr = Addr(regs.rsi);
    let flags = regs.rdx as u32;
    if flags & libc::O_CREAT as u32 != 0 {
        let mode = linux::FileMode(regs.r10 as u16);
        create_at(dir_fd, addr, flags, mode)
    } else {
        open_at(dir_fd, addr, flags)
    }
}

fn open_at(dir_fd: i32, addr: Addr, flags: u32) -> SysResult<usize> {
    let (path, is_dir_path) = copy_in_path(addr, false)?;
    let resolve = (flags as i32) & libc::O_NOFOLLOW == 0;
    let mut fd = 0;
    file_op_on(dir_fd, &path, resolve, |_, dirent, _| {
        let ctx = &*context::context();
        let file = {
            let dirent_ref = dirent.borrow();
            let inode = dirent_ref.inode();
            inode.check_permission(PermMask::from_linux_flags(flags), ctx)?;
            if inode.stable_attr().is_symlink() && !resolve {
                bail_libc!(libc::ELOOP);
            }

            let mut file_flags = FileFlags::from_linux_flags(flags as i32);
            file_flags.large_file = true;
            if inode.stable_attr().is_directory() {
                if file_flags.write {
                    bail_libc!(libc::EISDIR);
                }
            } else {
                if file_flags.directory {
                    bail_libc!(libc::ENOTDIR);
                }
                if is_dir_path {
                    bail_libc!(libc::ENOTDIR);
                }
            }
            let file = inode.get_file(dirent.clone(), file_flags)?;
            Rc::new(RefCell::new(file))
        };
        if flags as i32 & libc::O_TRUNC != 0 {
            let mut dirent = dirent.borrow_mut();
            dirent.inode_mut().truncate(0, ctx)?;
        }
        let new_fd = {
            let mut task = ctx.task_mut();
            task.new_fd_from(
                0,
                &file,
                FdFlags {
                    close_on_exec: flags as i32 & libc::O_CLOEXEC != 0,
                },
            )?
        };
        fd = new_fd as usize;
        Ok(())
    })?;
    Ok(fd)
}

fn create_at(dir_fd: i32, addr: Addr, flags: u32, mode: linux::FileMode) -> SysResult<usize> {
    let (path, is_dir_path) = copy_in_path(addr, false)?;
    if is_dir_path {
        bail_libc!(libc::ENOENT);
    }

    let mut file_flags = FileFlags::from_linux_flags(flags as i32);
    file_flags.large_file = true;

    enum Res {
        Ok(DirentRef),
        ErrReturnImmediate(SysError),
        ErrContinue(SysError),
    }

    let mut fd = 0;
    let ctx = &*context::context();

    file_op_at(dir_fd, &path, |root, parent, name, remaining_traversals| {
        let mut name = name.to_string();
        let mut parent = parent.clone();

        let mut res = || loop {
            let stable = parent.borrow().stable_attr();
            if !stable.is_directory() {
                return Res::ErrReturnImmediate(SysError::new(libc::ENOTDIR));
            }

            let task = ctx.task();
            let mount_namespace = task.mount_namespace();
            let found = match mount_namespace.find_link(
                root,
                Some(parent.clone()),
                &name,
                remaining_traversals,
                ctx,
            ) {
                Ok(v) => v,
                Err(err) => return Res::ErrContinue(err),
            };

            if flags as i32 & libc::O_EXCL != 0 {
                return Res::ErrReturnImmediate(SysError::new(libc::EEXIST));
            }

            let dirent = found.borrow();
            let inode = dirent.inode();
            if !inode.stable_attr().is_symlink() {
                drop(dirent);
                return Res::Ok(found);
            }

            if flags as i32 & libc::O_NOFOLLOW != 0 {
                return Res::ErrReturnImmediate(SysError::new(libc::ELOOP));
            }

            match inode.get_link() {
                Ok(_) => {
                    drop(dirent);
                    return Res::Ok(found);
                }
                Err(err) => {
                    if err.kind() != SysErrorKind::ErrResolveViaReadLink {
                        return Res::ErrReturnImmediate(err);
                    }
                    if *remaining_traversals == 0 {
                        return Res::ErrReturnImmediate(SysError::new(libc::ELOOP));
                    }
                    let path = match inode.read_link() {
                        Ok(p) => p,
                        Err(err) => return Res::ErrReturnImmediate(err),
                    };
                    *remaining_traversals -= 1;

                    let (new_parent_path, new_name) = fs::utils::split_last(&path);
                    let new_parent = match mount_namespace.find_inode(
                        root,
                        Some(parent.clone()),
                        new_parent_path,
                        remaining_traversals,
                        ctx,
                    ) {
                        Ok(p) => p,
                        Err(err) => return Res::ErrContinue(err),
                    };
                    parent = new_parent;
                    name = new_name.to_string();
                }
            }
        };

        let (_found, new_file) = match res() {
            Res::Ok(found) => {
                {
                    let dirent = found.borrow();
                    dirent
                        .inode()
                        .check_permission(PermMask::from_linux_flags(flags), ctx)?;
                }
                if flags as i32 & libc::O_TRUNC != 0 {
                    let mut dirent = found.borrow_mut();
                    dirent.inode_mut().truncate(0, ctx)?;
                }
                let nf = {
                    let dirent = found.borrow();
                    dirent.inode().get_file(found.clone(), file_flags)?
                };
                (found, nf)
            }
            Res::ErrReturnImmediate(err) => {
                return Err(err);
            }
            Res::ErrContinue(err) => match err.code() {
                libc::ENOENT => {
                    {
                        let dirent = parent.borrow();
                        dirent.inode().check_permission(
                            PermMask {
                                read: false,
                                write: true,
                                execute: true,
                            },
                            ctx,
                        )?;
                    }
                    let perms =
                        FilePermissions::from_mode(linux::FileMode(mode.0 & !(ctx.umask() as u16)));
                    let parent_ptr = parent.clone();
                    let nf = parent
                        .borrow_mut()
                        .create(root, &name, file_flags, perms, parent_ptr, ctx)?;
                    (nf.dirent(), nf)
                }
                _ => return Err(err),
            },
        };

        let mut task = ctx.task_mut();
        let new_fd = task.new_fd_from(
            0,
            &Rc::new(RefCell::new(new_file)),
            FdFlags {
                close_on_exec: flags as i32 & libc::O_CLOEXEC != 0,
            },
        )?;
        fd = new_fd as usize;
        Ok(())
    })?;
    Ok(fd)
}

fn file_op_at<F: FnMut(&DirentRef, &DirentRef, &str, &mut u32) -> SysResult<()>>(
    dir_fd: i32,
    path: &str,
    mut f: F,
) -> SysResult<()> {
    let (dir, name) = fs::utils::split_last(path);
    let ctx = &*context::context();
    let mut remaining_traversals = linux::MAX_SYMLINK_TRAVERSALS;
    let root = ctx.root_directory();
    match dir {
        "/" => f(root, root, name, &mut remaining_traversals),
        "." if dir_fd == libc::AT_FDCWD => {
            let wd = ctx.working_directory();
            f(root, wd, name, &mut remaining_traversals)
        }
        _ => file_op_on(dir_fd, dir, true, |root, d, remaining_traversals| {
            f(root, d, name, remaining_traversals)
        }),
    }
}

pub fn file_op_on<F: FnMut(&DirentRef, &DirentRef, &mut u32) -> SysResult<()>>(
    dir_fd: i32,
    path: &str,
    resolve: bool,
    mut f: F,
) -> SysResult<()> {
    let ctx = context::context();
    let rel = if path.starts_with('/') {
        None
    } else if dir_fd == libc::AT_FDCWD {
        Some(ctx.working_directory().clone())
    } else {
        let file = {
            let mut task = ctx.task_mut();
            task.get_file(dir_fd)
                .ok_or_else(|| SysError::new(libc::EBADF))?
        };
        let file = file.borrow();
        let dirent = file.dirent();
        if !dirent.borrow().stable_attr().is_directory() {
            bail_libc!(libc::ENOTDIR);
        }
        Some(dirent)
    };

    let root = ctx.root_directory();
    let mut remaining_traversals = linux::MAX_SYMLINK_TRAVERSALS as u32;
    let mount_namespace = {
        let task = ctx.task();
        task.mount_namespace().clone()
    };
    let dirent = if resolve {
        mount_namespace.find_inode(root, rel, path, &mut remaining_traversals, &*ctx)?
    } else {
        mount_namespace.find_link(root, rel, path, &mut remaining_traversals, &*ctx)?
    };
    f(root, &dirent, &mut remaining_traversals)
}

// returns: (path string, is directory)
pub fn copy_in_path(addr: Addr, allow_empty: bool) -> SysResult<(String, bool)> {
    let mut path = {
        let ctx = context::context();
        let mut task = ctx.task_mut();
        task.copy_in_string(addr, libc::PATH_MAX as usize)?
    };
    logger::info!("copy_in_path: {:?}", path);
    if path.is_empty() && !allow_empty {
        bail_libc!(libc::ENOENT);
    }
    let orig = path.clone();
    if path != "/" {
        path = path.trim_end_matches('/').to_string()
    };
    let changed = path != orig;
    Ok((path, changed))
}

// access implements linux syscall access(2)
pub fn access(regs: &libc::user_regs_struct) -> super::Result {
    let addr = Addr(regs.rdi);
    let mode = regs.rsi as u32;
    access_at(libc::AT_FDCWD, addr, mode).map(|()| 0)
}

fn access_at(dir_fd: i32, addr: Addr, mode: u32) -> SysResult<()> {
    const R_OK: u32 = 4;
    const W_OK: u32 = 2;
    const X_OK: u32 = 1;

    let (path, _) = copy_in_path(addr, false)?;

    if mode & !(R_OK | W_OK | X_OK) != 0 {
        bail_libc!(libc::EINVAL);
    }

    file_op_on(dir_fd, &path, true, |_, dirent, _| {
        let ctx = &*context::context();
        let mut creds = ctx.credentials().clone();
        creds.effective_kuid = creds.real_kuid;
        creds.effective_kgid = creds.real_kgid;
        creds.effective_caps =
            if creds.user_namespace.map_from_kuid(&creds.effective_kuid) == Uid::root() {
                creds.permitted_caps
            } else {
                CapabilitySet(0)
            };
        let dirent = dirent.borrow();
        dirent.inode().check_permission(
            PermMask {
                read: mode & R_OK != 0,
                write: mode & W_OK != 0,
                execute: mode & X_OK != 0,
            },
            ctx,
        )
    })
}

// close implements linux syscall close(2)
pub fn close(regs: &libc::user_regs_struct) -> super::Result {
    let fd = regs.rdi as i32;
    let ctx = context::context();
    let mut task = ctx.task_mut();
    let f = task
        .fd_table_mut()
        .remove(fd)
        .ok_or_else(|| SysError::new(libc::EBADF))?;

    f.borrow()
        .flush()
        .expect("flush returned error in current implementation?");
    f.borrow().close()?;

    Ok(0)
}

// ioctl implements linux syscall ioctl(2)
pub fn ioctl(regs: &libc::user_regs_struct) -> super::Result {
    let fd = regs.rdi as i32;
    let request = regs.rsi;

    let ctx = &*context::context();
    let mut task = ctx.task_mut();
    let file = task
        .get_file(fd)
        .ok_or_else(|| SysError::new(libc::EBADF))?;
    match request {
        linux::FIONCLEX => {
            task.fd_table_mut()
                .set_flags(
                    fd,
                    FdFlags {
                        close_on_exec: false,
                    },
                )
                .unwrap();
            Ok(0)
        }
        linux::FIOCLEX => {
            task.fd_table_mut()
                .set_flags(
                    fd,
                    FdFlags {
                        close_on_exec: true,
                    },
                )
                .unwrap();
            Ok(0)
        }
        linux::FIONBIO => {
            let mut dst = [0; 4];
            task.copy_in_bytes(Addr(regs.rdx), &mut dst)?;
            let mut flags = *file.as_ref().borrow().flags();
            let set = u32::from_le_bytes(dst);
            flags.non_blocking = set != 0;
            file.as_ref().borrow_mut().set_flags(flags.as_settable());
            Ok(0)
        }
        linux::FIOASYNC => {
            let mut dst = [0; 4];
            task.copy_in_bytes(Addr(regs.rdx), &mut dst)?;
            let mut flags = *file.as_ref().borrow().flags();
            let set = u32::from_le_bytes(dst);
            flags.async_ = set != 0;
            file.as_ref().borrow_mut().set_flags(flags.as_settable());
            Ok(0)
        }
        linux::FIOSETOWN | linux::SIOCSPGRP => todo!("This flags is not implemented yet"),
        linux::FIOGETOWN | linux::SIOCGPGRP => todo!("This flags is not implemented yet"),
        _ => {
            let file = file.as_ref().borrow();
            drop(task);
            file.ioctl(regs, ctx)
        }
    }
}

// readlink implements linux syscall readlink(2)
pub fn readlink(regs: &libc::user_regs_struct) -> super::Result {
    let addr = Addr(regs.rdi);
    let buf_addr = Addr(regs.rsi);
    let size = regs.rdx as u32;
    readlink_at(libc::AT_FDCWD, addr, buf_addr, size)
}

fn readlink_at(dir_fd: i32, addr: Addr, buf_addr: Addr, size: u32) -> SysResult<usize> {
    let (path, is_dir) = copy_in_path(addr, false)?;
    if is_dir {
        bail_libc!(libc::ENOENT);
    }
    let mut copied = 0;
    file_op_on(dir_fd, &path, false, |_, d, _| {
        let d_ref = d.borrow();
        let inode = d_ref.inode();
        let ctx = &*context::context();
        inode.check_permission(
            PermMask {
                read: true,
                write: false,
                execute: false,
            },
            ctx,
        )?;
        let s = match inode.read_link() {
            Ok(s) => s,
            Err(err) if err.code() == libc::ENOLINK => bail_libc!(libc::EINVAL),
            Err(err) => return Err(err),
        };

        let mut buf = s.as_bytes();
        if buf.len() > size as usize {
            buf = &buf[..size as usize];
        }

        let task = ctx.task();
        copied = task.copy_out_bytes(buf_addr, buf)?;
        Ok(())
    })?;
    Ok(copied)
}

// fcntl implements linux syscall fcntl(2)
pub fn fcntl(regs: &libc::user_regs_struct) -> super::Result {
    let fd = regs.rdi as i32;
    let cmd = regs.rsi as i32;

    let ctx = context::context();
    let (file, flags) = {
        let task = ctx.task();
        let fd_table = task.fd_table();
        fd_table.get(fd).ok_or_else(|| SysError::new(libc::EBADF))?
    };

    match cmd {
        libc::F_GETFD => Ok(flags.as_linux_fd_flags() as usize),
        libc::F_SETFD => {
            let flags = regs.rdx as i32;
            let mut task = ctx.task_mut();
            let fd_tables = task.fd_table_mut();
            fd_tables
                .set_flags(
                    fd,
                    FdFlags {
                        close_on_exec: flags & libc::FD_CLOEXEC != 0,
                    },
                )
                .map(|()| 0)
        }
        libc::F_GETFL => Ok(file.borrow().flags().to_linux_flags() as usize),
        libc::F_SETFL => {
            let flags = regs.rdx as i32;
            file.borrow_mut()
                .set_flags(FileFlags::from_linux_flags(flags).as_settable());
            Ok(0)
        }
        libc::F_DUPFD | libc::F_DUPFD_CLOEXEC => {
            let from = regs.rdx as i32;
            let mut task = ctx.task_mut();
            task.new_fd_from(
                from,
                &file,
                FdFlags {
                    close_on_exec: cmd == libc::F_DUPFD_CLOEXEC,
                },
            )
            .map(|fd| fd as usize)
        }
        _ => todo!("the flag {} is yet to be implemented in fcntl(2)", cmd),
    }
}

// rename implements linux syscall rename(2)
pub fn rename(regs: &libc::user_regs_struct) -> super::Result {
    let old_path_addr = Addr(regs.rdi);
    let new_path_addr = Addr(regs.rsi);
    rename_at(libc::AT_FDCWD, old_path_addr, libc::AT_FDCWD, new_path_addr).map(|()| 0)
}

// renameat implements linux syscall renameat(2)
pub fn renameat(regs: &libc::user_regs_struct) -> super::Result {
    let old_dir_fd = regs.rdi as i32;
    let old_path_addr = Addr(regs.rsi);
    let new_dir_fd = regs.rdx as i32;
    let new_path_addr = Addr(regs.r10);
    rename_at(old_dir_fd, old_path_addr, new_dir_fd, new_path_addr).map(|()| 0)
}

fn rename_at(old_dir_fd: i32, old_addr: Addr, new_dir_fd: i32, new_addr: Addr) -> SysResult<()> {
    let (old_path, _) = copy_in_path(old_addr, false)?;
    let (new_path, _) = copy_in_path(new_addr, false)?;

    file_op_at(old_dir_fd, &old_path, |_, old_parent, old_name, _| {
        if !old_parent.borrow().stable_attr().is_directory() {
            bail_libc!(libc::ENOTDIR);
        }
        if old_name.is_empty() || old_name == "." || old_name == ".." {
            bail_libc!(libc::EBUSY);
        }

        file_op_at(new_dir_fd, &new_path, |root, new_parent, new_name, _| {
            if !new_parent.borrow().stable_attr().is_directory() {
                bail_libc!(libc::ENOTDIR);
            }
            if new_name.is_empty() || new_name == "." || new_name == ".." {
                bail_libc!(libc::EBUSY);
            }
            let ctx = &*context::context();
            fs::rename(
                root,
                old_parent,
                Component::Normal(old_name.as_ref()),
                new_parent,
                new_name.to_string(),
                ctx,
            )
        })
    })
}

// dup implements linux syscall dup(2)
pub fn dup(regs: &libc::user_regs_struct) -> super::Result {
    let fd = regs.rdi as i32;
    let ctx = context::context();
    let mut task = ctx.task_mut();
    let file = task
        .get_file(fd)
        .ok_or_else(|| SysError::new(libc::EBADF))?;
    task.new_fd_from(
        0,
        &file,
        FdFlags {
            close_on_exec: false,
        },
    )
    .map(|fd| fd as usize)
}
