use std::{
    path::{Component, Path},
    rc::Rc,
};

use utils::{bail_libc, err_libc, SysError, SysErrorKind, SysResult};

use crate::{attr::PermMask, DirentRef};

use super::context::Context;

#[derive(Clone, Copy, Default, Debug)]
pub struct MountSourceFlags {
    pub read_only: bool,
    pub no_atime: bool,
    pub force_page_cache: bool,
    pub no_exec: bool,
}

#[derive(Debug)]
pub struct MountSource {
    flags: MountSourceFlags,
}

impl MountSource {
    pub fn new(flags: MountSourceFlags) -> Self {
        Self { flags }
    }

    pub fn new_pseudo() -> Self {
        Self::new(MountSourceFlags::default())
    }

    pub fn new_non_caching(flags: MountSourceFlags) -> Self {
        Self::new(flags)
    }

    pub fn flags(&self) -> MountSourceFlags {
        self.flags
    }
}

#[derive(Debug, Clone)]
pub struct MountNamespace {
    root: DirentRef,
}

impl MountNamespace {
    pub fn new(root: DirentRef) -> Self {
        Self { root }
    }

    pub fn find_inode<P: AsRef<Path>>(
        &self,
        root: &DirentRef,
        wd: Option<DirentRef>,
        path: P,
        remaining_traversals: &mut u32,
        ctx: &dyn Context,
    ) -> SysResult<DirentRef> {
        let dirent = self.find_link(root, wd, path, remaining_traversals, ctx)?;
        self.resolve(root, dirent, remaining_traversals, ctx)
    }

    pub fn find_link<P: AsRef<Path>>(
        &self,
        root: &DirentRef,
        wd: Option<DirentRef>,
        path: P,
        remaining_traversals: &mut u32,
        ctx: &dyn Context,
    ) -> SysResult<DirentRef> {
        if path.as_ref().to_str().unwrap().is_empty() {
            panic!("MountNamespace.find_link: path is empty");
        }

        let mut current = wd.unwrap_or_else(|| root.clone());
        let mut components = path.as_ref().components();
        let mut first = match components
            .next()
            .expect("path is not empty and no components?")
        {
            Component::RootDir => match components.next() {
                Some(c) => {
                    current = root.clone();
                    c
                }
                None => return Ok(root.clone()),
            },
            c => c,
        };

        loop {
            if Rc::as_ptr(&current) != Rc::as_ptr(root) {
                let current = current.borrow();
                let inode = current.inode();
                if !inode.stable_attr().is_directory() {
                    bail_libc!(libc::ENOTDIR);
                }
                inode.check_permission(
                    PermMask {
                        read: false,
                        write: false,
                        execute: true,
                    },
                    ctx,
                )?;
            }
            let cloned = Rc::clone(&current);
            let next = current.borrow_mut().walk(root, first, cloned, ctx)?;

            first = match components.next() {
                None => return Ok(next),
                Some(c) => {
                    current = self.resolve(root, next, remaining_traversals, ctx)?;
                    c
                }
            };
        }
    }

    // resolve resolves the given link
    fn resolve(
        &self,
        root: &DirentRef,
        node: DirentRef,
        remaining_traversals: &mut u32,
        ctx: &dyn Context,
    ) -> SysResult<DirentRef> {
        let dirent = node.borrow();
        let inode = dirent.inode();
        match inode.get_link() {
            Ok(target) => {
                if *remaining_traversals == 0 {
                    err_libc!(libc::ELOOP)
                } else {
                    Ok(target)
                }
            }
            Err(err) => {
                if err.code() == libc::ENOLINK {
                    drop(dirent);
                    Ok(node)
                } else if err.kind() == SysErrorKind::ErrResolveViaReadLink {
                    if *remaining_traversals == 0 {
                        err_libc!(libc::ELOOP)
                    } else {
                        let target_path = inode.read_link()?;
                        let parent = dirent.parent().upgrade().unwrap();
                        *remaining_traversals -= 1;
                        self.find_inode(root, Some(parent), &target_path, remaining_traversals, ctx)
                    }
                } else {
                    Err(err)
                }
            }
        }
    }

    pub fn root(&self) -> &DirentRef {
        &self.root
    }
}
