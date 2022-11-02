use std::{
    cell::RefCell,
    error::Error,
    path::{Component, Path},
    rc::{Rc, Weak},
};

use memmap::Mappable;
use utils::{bail_libc, err_libc, SysError, SysResult};

use crate::{
    attr::{PermMask, StableAttr, UnstableAttr},
    dentry::{DentAttr, DirIterCtx},
    file::FILE_MAX_OFFSET,
    inode_operations::RenameUnderParents,
    DirentRef, DirentWeakRef, File,
};

use super::{attr::FilePermissions, context::Context, inode::Inode, FileFlags};

#[derive(Debug)]
pub struct Dirent {
    inode: Inode,
    name: String,
    parent: DirentWeakRef,
    mounted: bool,
    // TODO: define children to cache dirents
}

unsafe impl Send for Dirent {}
unsafe impl Sync for Dirent {}

impl Dirent {
    pub fn new(inode: Inode, name: String) -> DirentRef {
        let dirent = Self {
            inode,
            name,
            parent: Weak::new(),
            mounted: false,
        };
        Rc::new(RefCell::new(dirent))
    }

    #[inline]
    pub fn parent(&self) -> &DirentWeakRef {
        &self.parent
    }

    #[inline]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[inline]
    pub fn inode(&self) -> &Inode {
        &self.inode
    }

    #[inline]
    pub fn inode_mut(&mut self) -> &mut Inode {
        &mut self.inode
    }

    #[inline]
    fn is_root(&self) -> bool {
        self.parent.upgrade().is_none()
    }

    fn can_delete(&self, victim: &DirentRef, ctx: &dyn Context) -> SysResult<()> {
        self.inode.check_sticky(victim.borrow().inode(), ctx)?;
        if victim.borrow().is_root() {
            err_libc!(libc::EBUSY)
        } else {
            Ok(())
        }
    }

    fn is_mount_point_locked(&self) -> bool {
        self.mounted || self.is_root()
    }

    #[inline]
    pub fn stable_attr(&self) -> StableAttr {
        self.inode.stable_attr()
    }

    #[inline]
    pub fn unstable_attr(&self) -> SysResult<UnstableAttr> {
        self.inode.unstable_attr()
    }

    pub fn walk(
        &mut self,
        root: &DirentRef,
        name: Component,
        self_ptr: DirentRef,
        ctx: &dyn Context,
    ) -> SysResult<DirentRef> {
        if !self.stable_attr().is_directory() {
            bail_libc!(libc::ENOTDIR);
        }

        match name {
            Component::RootDir => panic!("walking a root directory is impossible."),
            Component::Prefix(_) => panic!("impossible to occur in unix system"),
            Component::CurDir => Ok(self_ptr),
            Component::ParentDir => {
                if Rc::as_ptr(&self_ptr) == Rc::as_ptr(root) {
                    Ok(self_ptr)
                } else {
                    match self.parent.upgrade() {
                        Some(ref parent) => Ok(parent.clone()),
                        None => Ok(self_ptr), // self is root.
                    }
                }
            }
            Component::Normal(name) => {
                let name = name.to_str().unwrap();
                let c = self.inode.lookup(name, ctx)?;
                let mut c_dir = c.borrow_mut();
                if c_dir.name() != name {
                    panic!(
                        "lookup from {} to {} returned unexpected name {}",
                        c_dir.name(),
                        name,
                        c_dir.name(),
                    );
                }

                c_dir.parent = Rc::downgrade(&self_ptr);
                Ok(c.clone())
            }
        }
    }

    pub fn exists(
        &mut self,
        root: &DirentRef,
        name: &str,
        self_ptr: DirentRef,
        ctx: &dyn Context,
    ) -> bool {
        let name = match Path::new(name).components().next() {
            Some(n) => n,
            None => return true,
        };
        self.walk(root, name, self_ptr, ctx).is_ok()
    }

    pub fn create(
        &mut self,
        root: &DirentRef,
        name: &str,
        flags: FileFlags,
        perms: FilePermissions,
        self_ptr: DirentRef,
        ctx: &dyn Context,
    ) -> SysResult<File> {
        if self.exists(root, name, self_ptr, ctx) {
            bail_libc!(libc::EEXIST);
        }
        let parent_uattr = self.inode.unstable_attr()?;
        let msrc = self.inode.mount_source().clone();
        let file = self
            .inode
            .create(name, flags, perms, parent_uattr, msrc, ctx)?;
        let child = file.dirent();
        self.finish_create(child, name);
        Ok(file)
    }

    fn finish_create(&self, child: DirentRef, name: &str) {
        if child.borrow().name() != name {
            panic!(
                "create from {} to {} returned unexpected name: {}",
                self.name,
                name,
                child.borrow().name
            );
        }
    }
}

impl Mappable for Dirent {
    fn translate(
        &self,
        _: memmap::MappableRange,
        _: memmap::MappableRange,
        _: mem::AccessType,
    ) -> (Vec<memmap::Translation>, SysResult<()>) {
        todo!();
    }
    fn add_mapping(&mut self, _ar: mem::AddrRange, _offset: u64, _writable: bool) -> SysResult<()> {
        todo!();
    }
    fn remove_mapping(&mut self, _ar: mem::AddrRange, _offset: u64, _writable: bool) {
        todo!();
    }
    fn copy_mapping(
        &mut self,
        _: mem::AddrRange,
        _: mem::AddrRange,
        _: u64,
        _: bool,
    ) -> SysResult<()> {
        todo!();
    }
}

pub trait DirentOperations {
    fn is_descendant_of(&self, p: &DirentRef) -> bool;
}

impl DirentOperations for DirentRef {
    fn is_descendant_of(&self, p: &DirentRef) -> bool {
        if Rc::as_ptr(self) == Rc::as_ptr(p) {
            true
        } else if self.borrow().is_root() {
            false
        } else {
            self.borrow()
                .parent()
                .upgrade()
                .unwrap()
                .is_descendant_of(p)
        }
    }
}

fn get_dot_attrs(d: &DirentRef, root: &DirentRef) -> (DentAttr, DentAttr) {
    // get '.'
    let d_ref = d.borrow();
    let inode = d_ref.inode();
    let sattr = inode.stable_attr();
    let dot = DentAttr {
        typ: sattr.typ,
        inode_id: sattr.inode_id,
    };

    // get '..'
    if !d_ref.is_root() && d.is_descendant_of(root) {
        let psattr = d_ref
            .parent
            .upgrade()
            .unwrap()
            .borrow()
            .inode()
            .stable_attr();
        let dotdot = DentAttr {
            typ: psattr.typ,
            inode_id: psattr.inode_id,
        };
        (dot, dotdot)
    } else {
        (dot, dot)
    }
}

pub type ReaddirResult<T> = std::result::Result<T, ReaddirError<T>>;

pub trait DirIterator {
    fn iterate_dir(
        &self,
        inode: &mut Inode,
        dir_ctx: &mut DirIterCtx,
        offset: i32,
        ctx: &dyn Context,
    ) -> ReaddirResult<i32>;
}

#[derive(Debug, Clone, Copy)]
pub struct ReaddirError<T> {
    value: T,
    code: i32,
}

impl<T: Copy + Clone> ReaddirError<T> {
    pub fn new(value: T, code: i32) -> Self {
        Self { value, code }
    }

    #[inline]
    pub fn value(&self) -> T {
        self.value
    }

    #[inline]
    pub fn code(&self) -> i32 {
        self.code
    }
}

impl<T: std::fmt::Display> std::fmt::Display for ReaddirError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Error occurred while readdir: ok until {}", self.value)
    }
}

impl<T: std::fmt::Debug + std::fmt::Display> Error for ReaddirError<T> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        None
    }
}

pub fn dirent_readdir(
    d: &DirentRef,
    it: &dyn DirIterator,
    root: &DirentRef,
    offset: i64,
    dir_ctx: &mut DirIterCtx,
    ctx: &dyn Context,
) -> ReaddirResult<i64> {
    let offset = dirent_readdir_impl(d, it, root, offset, dir_ctx, ctx);
    if dir_ctx.serializer.written_bytes() > 0 {
        Ok(offset.unwrap_or_else(|o| o.value))
    } else {
        offset
    }
}

fn dirent_readdir_impl(
    d: &DirentRef,
    it: &dyn DirIterator,
    root: &DirentRef,
    mut offset: i64,
    dir_ctx: &mut DirIterCtx,
    ctx: &dyn Context,
) -> ReaddirResult<i64> {
    let d_ref = d.borrow();
    if !d_ref.inode.stable_attr().is_directory() {
        return Err(ReaddirError::new(0, libc::ENOTDIR));
    }
    if offset == FILE_MAX_OFFSET {
        return Ok(offset);
    }
    let (dot, dotdot) = get_dot_attrs(d, root);
    if offset == 0 {
        dir_ctx
            .dir_emit(".".to_string(), dot)
            .map_err(|e| ReaddirError::new(offset, e.raw_os_error().unwrap_or(-1)))?;
        offset += 1;
    }
    if offset == 1 {
        dir_ctx
            .dir_emit("..".to_string(), dotdot)
            .map_err(|e| ReaddirError::new(offset, e.raw_os_error().unwrap_or(-1)))?;
        offset += 1;
    }
    offset -= 2;
    drop(d_ref);
    let mut dirent = d.borrow_mut();
    match it.iterate_dir(dirent.inode_mut(), dir_ctx, offset as i32, ctx) {
        Ok(new_offset) if (new_offset as i64) < offset => panic!(
            "readdir returned offset {} less than input offset {}",
            new_offset, offset
        ),
        Ok(new_offset) => Ok((new_offset as i64) + 2),
        Err(err) => Err(ReaddirError::new((err.value as i64) + 2, err.code)),
    }
}

pub fn rename(
    root: &DirentRef,
    old_parent: &DirentRef,
    old_name: Component,
    new_parent: &DirentRef,
    new_name: String,
    ctx: &dyn Context,
) -> SysResult<()> {
    let new_name_component = Component::Normal(new_name.as_ref());
    if Rc::as_ptr(old_parent) == Rc::as_ptr(new_parent) {
        return rename_in_same_parent(root, old_parent, old_name, new_name, ctx);
    }
    {
        let old_parent = old_parent.borrow();
        let new_parent = new_parent.borrow();
        let mask = PermMask {
            read: false,
            write: true,
            execute: true,
        };
        old_parent.inode.check_permission(mask, ctx)?;
        new_parent.inode.check_permission(mask, ctx)?;
    }
    let renamed = old_parent
        .borrow_mut()
        .walk(root, old_name, old_parent.clone(), ctx)?;
    old_parent.borrow().can_delete(&renamed, ctx)?;

    if renamed.borrow().is_mount_point_locked() {
        bail_libc!(libc::EBUSY)
    }

    if new_parent.is_descendant_of(&renamed) {
        bail_libc!(libc::EINVAL)
    }

    let renamed_ptr = renamed.borrow();
    let renamed_name = &renamed_ptr.name;
    let renamed_inode = &renamed_ptr.inode;
    let renamed_is_dir = renamed_inode.stable_attr().is_directory();
    if renamed_is_dir {
        renamed_inode.check_permission(
            PermMask {
                read: false,
                write: true,
                execute: false,
            },
            ctx,
        )?;
    }

    {
        let cloned = new_parent.clone();
        let mut new_parent = new_parent.borrow_mut();

        let is_replaced = match new_parent.walk(root, new_name_component, cloned, ctx) {
            Ok(d) => {
                new_parent.can_delete(&d, ctx)?;
                if old_parent.is_descendant_of(&d) {
                    bail_libc!(libc::ENOTEMPTY);
                }
                if d.borrow().is_mount_point_locked() {
                    bail_libc!(libc::EBUSY);
                }
                let new_is_dir = d.borrow().stable_attr().is_directory();
                if !new_is_dir && renamed_is_dir {
                    bail_libc!(libc::ENOTDIR);
                }
                if !renamed_is_dir && new_is_dir {
                    bail_libc!(libc::EISDIR);
                }
                Some(d)
            }
            Err(err) if err.code() == libc::ENOENT => None,
            Err(err) => return Err(err),
        }
        .is_some();

        let mut old_parent = old_parent.borrow_mut();
        let parents = RenameUnderParents::Different {
            old: old_parent.inode_mut(),
            new: new_parent.inode_mut(),
        };
        renamed_inode.rename(parents, renamed_name, new_name.clone(), is_replaced, ctx)?;
    }

    drop(renamed_ptr);
    let mut renamed = renamed.borrow_mut();
    renamed.name = new_name;
    renamed.parent = Rc::downgrade(new_parent);

    Ok(())
}

fn rename_in_same_parent(
    root: &DirentRef,
    parent: &DirentRef,
    old_name: Component,
    new_name: String,
    ctx: &dyn Context,
) -> SysResult<()> {
    let new_name_component = Component::Normal(new_name.as_ref());
    if old_name == new_name_component {
        return Ok(());
    }
    {
        let parent = parent.borrow();
        let mask = PermMask {
            read: false,
            write: true,
            execute: true,
        };
        parent.inode.check_permission(mask, ctx)?;
    }
    let renamed = parent
        .borrow_mut()
        .walk(root, old_name, parent.clone(), ctx)?;
    parent.borrow().can_delete(&renamed, ctx)?;

    if renamed.borrow().is_mount_point_locked() {
        bail_libc!(libc::EBUSY)
    }

    if parent.is_descendant_of(&renamed) {
        bail_libc!(libc::EINVAL)
    }

    let renamed_ptr = renamed.borrow();
    let renamed_name = &renamed_ptr.name;
    let renamed_inode = &renamed_ptr.inode;
    let renamed_is_dir = renamed_inode.stable_attr().is_directory();
    if renamed_is_dir {
        renamed_inode.check_permission(
            PermMask {
                read: false,
                write: true,
                execute: false,
            },
            ctx,
        )?;
    }

    {
        let cloned = parent.clone();
        let mut parent_mut = parent.borrow_mut();

        let is_replaced = match parent_mut.walk(root, new_name_component, cloned, ctx) {
            Ok(d) => {
                parent_mut.can_delete(&d, ctx)?;
                if parent.is_descendant_of(&d) {
                    bail_libc!(libc::ENOTEMPTY);
                }
                if d.borrow().is_mount_point_locked() {
                    bail_libc!(libc::EBUSY);
                }
                let new_is_dir = d.borrow().stable_attr().is_directory();
                if !new_is_dir && renamed_is_dir {
                    bail_libc!(libc::ENOTDIR);
                }
                if !renamed_is_dir && new_is_dir {
                    bail_libc!(libc::EISDIR);
                }
                Some(d)
            }
            Err(err) if err.code() == libc::ENOENT => None,
            Err(err) => return Err(err),
        }
        .is_some();

        renamed_inode.rename(
            RenameUnderParents::Same(&mut parent_mut.inode),
            renamed_name,
            new_name.clone(),
            is_replaced,
            ctx,
        )?;
    }

    drop(renamed_ptr);
    let mut renamed = renamed.borrow_mut();
    renamed.name = new_name;
    renamed.parent = Rc::downgrade(parent);

    Ok(())
}
