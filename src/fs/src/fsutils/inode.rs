use std::{rc::Rc, sync::RwLock};

use time::Time;
use utils::{bail_libc, SysError, SysResult};

use crate::{
    attr::{FileOwner, FilePermissions, StableAttr, UnstableAttr},
    context::Context,
    inode,
    inode_operations::RenameUnderParents,
    mount::MountSource,
    DirentRef, File, FileFlags, InodeOperations,
};

#[derive(Debug)]
pub struct InodeSimpleAttributes {
    pub fs_type: u64,
    pub uattr: RwLock<UnstableAttr>,
}

impl InodeSimpleAttributes {
    pub fn new<F: Fn() -> Time>(
        owner: FileOwner,
        perms: FilePermissions,
        typ: u64,
        timer: F,
    ) -> Self {
        let uattr = UnstableAttr {
            owner,
            perms,
            ..UnstableAttr::default()
        };
        let uattr = uattr.record_current_time(timer);
        Self::new_with_unstable(uattr, typ)
    }

    pub fn new_with_unstable(uattr: UnstableAttr, fs_type: u64) -> Self {
        Self {
            fs_type,
            uattr: RwLock::new(uattr),
        }
    }
}

impl InodeOperations for InodeSimpleAttributes {
    fn lookup(&mut self, _: &str, _: &dyn Context) -> SysResult<DirentRef> {
        bail_libc!(libc::ENOTDIR)
    }
    fn get_file(&self, _: DirentRef, _: FileFlags) -> SysResult<File> {
        bail_libc!(libc::EIO)
    }
    fn unstable_attr(&self, _: &Rc<MountSource>, _: StableAttr) -> SysResult<UnstableAttr> {
        Ok(*self.uattr.read().unwrap())
    }
    fn get_link(&self) -> SysResult<DirentRef> {
        bail_libc!(libc::ENOLINK)
    }
    fn read_link(&self) -> SysResult<String> {
        bail_libc!(libc::ENOLINK)
    }
    fn truncate(&mut self, _: i64, _: &dyn Context) -> SysResult<()> {
        bail_libc!(libc::EINVAL)
    }
    fn create(
        &mut self,
        _: UnstableAttr,
        _: Rc<MountSource>,
        _: &str,
        _: FileFlags,
        _: FilePermissions,
        _: &dyn Context,
    ) -> SysResult<File> {
        bail_libc!(libc::ENOTDIR)
    }
    fn rename(
        &self,
        _: RenameUnderParents<&mut inode::Inode>,
        _: &str,
        _: String,
        _: bool,
        _: &dyn Context,
    ) -> SysResult<()> {
        logger::warn!("renaming is only allowed for the files that were created by user");
        bail_libc!(libc::EPERM)
    }
    fn add_link(&self) {
        self.uattr.write().unwrap().links += 1;
    }
    fn drop_link(&self) {
        self.uattr.write().unwrap().links -= 1;
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[derive(Debug)]
pub struct SimpleFileInode {
    pub attrs: InodeSimpleAttributes,
}

impl SimpleFileInode {
    pub fn new<F: Fn() -> Time>(
        owner: FileOwner,
        perms: FilePermissions,
        typ: u64,
        timer: F,
    ) -> Self {
        Self {
            attrs: InodeSimpleAttributes::new(owner, perms, typ, timer),
        }
    }
}

impl InodeOperations for SimpleFileInode {
    fn lookup(&mut self, _: &str, _: &dyn Context) -> SysResult<DirentRef> {
        bail_libc!(libc::ENOTDIR)
    }
    fn get_file(&self, _: DirentRef, _: FileFlags) -> SysResult<File> {
        bail_libc!(libc::EIO)
    }
    fn unstable_attr(&self, msrc: &Rc<MountSource>, sattr: StableAttr) -> SysResult<UnstableAttr> {
        self.attrs.unstable_attr(msrc, sattr)
    }
    fn get_link(&self) -> SysResult<DirentRef> {
        bail_libc!(libc::ENOLINK)
    }
    fn read_link(&self) -> SysResult<String> {
        bail_libc!(libc::ENOLINK)
    }
    fn truncate(&mut self, _: i64, _: &dyn Context) -> SysResult<()> {
        bail_libc!(libc::EINVAL)
    }
    fn create(
        &mut self,
        _: UnstableAttr,
        _: Rc<MountSource>,
        _: &str,
        _: FileFlags,
        _: FilePermissions,
        _: &dyn Context,
    ) -> SysResult<File> {
        bail_libc!(libc::ENOTDIR)
    }
    fn rename(
        &self,
        _: RenameUnderParents<&mut inode::Inode>,
        _: &str,
        _: String,
        _: bool,
        _: &dyn Context,
    ) -> SysResult<()> {
        bail_libc!(libc::ENOTDIR)
    }
    fn add_link(&self) {
        self.attrs.add_link()
    }
    fn drop_link(&self) {
        self.attrs.drop_link()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
