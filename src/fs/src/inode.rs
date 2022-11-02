use std::rc::Rc;

use dev::Device;
use linux::Capability;
use mem::PAGE_SIZE;
use time::Time;
use utils::{bail_libc, err_libc, SysError, SysResult};

use crate::{inode_operations::RenameUnderParents, DirentRef};

use super::{
    attr::{FileOwner, FilePermissions, InodeType, PermMask, StableAttr, UnstableAttr},
    context::Context,
    fsutils::inode::InodeSimpleAttributes,
    mount::MountSource,
    File, FileFlags, InodeOperations,
};

#[derive(Debug)]
pub struct Inode {
    inode_operations: Box<dyn InodeOperations>,
    stable_attr: StableAttr,
    mount_source: Rc<MountSource>,
}

impl Inode {
    pub fn new(
        inode_operations: Box<dyn InodeOperations>,
        mount_source: Rc<MountSource>,
        stable_attr: StableAttr,
    ) -> Self {
        Inode {
            inode_operations,
            stable_attr,
            mount_source,
        }
    }

    pub fn new_anon<F: Fn() -> Time>(timer: F) -> Self {
        let iops = InodeSimpleAttributes::new(
            FileOwner::root(),
            FilePermissions {
                user: PermMask {
                    read: true,
                    write: true,
                    execute: false,
                },
                ..FilePermissions::default()
            },
            linux::ANON_INODE_FS_MAGIC,
            timer,
        );

        Self::new(
            Box::new(iops),
            Rc::new(MountSource::new_pseudo()),
            StableAttr {
                typ: InodeType::Anonymous,
                device_id: Device::new_anonymous_device().lock().unwrap().device_id(),
                inode_id: Device::new_anonymous_device().lock().unwrap().next_ino(),
                block_size: PAGE_SIZE as i64,
                device_file_major: 0,
                device_file_minor: 0,
            },
        )
    }

    #[inline]
    pub fn mount_source(&self) -> &Rc<MountSource> {
        &self.mount_source
    }

    pub fn unstable_attr(&self) -> SysResult<UnstableAttr> {
        self.inode_operations
            .unstable_attr(&self.mount_source, self.stable_attr)
    }

    #[inline]
    pub fn stable_attr(&self) -> StableAttr {
        self.stable_attr
    }

    pub fn inode_operations<T: 'static>(&self) -> &T {
        let iops = &self.inode_operations;
        iops.as_any()
            .downcast_ref::<T>()
            .expect("failed to cast InodeOperations")
    }

    pub fn inode_operations_mut<T: 'static>(&mut self) -> &mut T {
        let iops = &mut self.inode_operations;
        iops.as_any_mut()
            .downcast_mut::<T>()
            .expect("failed to cast InodeOperations")
    }

    pub fn check_permission(&self, p: PermMask, ctx: &dyn Context) -> SysResult<()> {
        if p.write && self.mount_source.flags().read_only {
            bail_libc!(libc::EROFS);
        }
        if !ctx.can_access_file(self, p) {
            bail_libc!(libc::EACCES);
        }
        Ok(())
    }

    pub fn check_capability(&self, cp: &Capability, ctx: &dyn Context) -> bool {
        let uattr = match self.unstable_attr() {
            Ok(v) => v,
            Err(_) => return false,
        };
        let creds = ctx.credentials();
        if !creds.user_namespace.map_from_kuid(&uattr.owner.uid).is_ok() {
            false
        } else if !creds.user_namespace.map_from_kgid(&uattr.owner.gid).is_ok() {
            false
        } else {
            creds.has_capability(cp)
        }
    }

    pub fn get_file(&self, dirent: DirentRef, flags: FileFlags) -> SysResult<File> {
        self.inode_operations.get_file(dirent, flags)
    }

    pub fn lookup(&mut self, name: &str, ctx: &dyn Context) -> SysResult<DirentRef> {
        self.inode_operations.lookup(name, ctx)
    }

    pub fn get_link(&self) -> SysResult<DirentRef> {
        self.inode_operations.get_link()
    }

    pub fn read_link(&self) -> SysResult<String> {
        self.inode_operations.read_link()
    }

    pub fn truncate(&mut self, size: i64, ctx: &dyn Context) -> SysResult<()> {
        if self.stable_attr.is_directory() {
            bail_libc!(libc::EISDIR);
        }
        self.inode_operations.truncate(size, ctx)
    }

    pub fn create(
        &mut self,
        name: &str,
        flags: FileFlags,
        perms: FilePermissions,
        parent_uattr: UnstableAttr,
        mount_source: Rc<MountSource>,
        ctx: &dyn Context,
    ) -> SysResult<File> {
        self.inode_operations
            .create(parent_uattr, mount_source, name, flags, perms, ctx)
    }

    pub fn rename(
        &self,
        parents: RenameUnderParents<&mut Inode>,
        old_name: &str,
        new_name: String,
        is_replacement: bool,
        ctx: &dyn Context,
    ) -> SysResult<()> {
        self.inode_operations
            .rename(parents, old_name, new_name, is_replacement, ctx)
    }

    pub fn add_link(&self) {
        self.inode_operations.add_link()
    }

    pub fn drop_link(&self) {
        self.inode_operations.drop_link()
    }

    pub fn check_sticky(&self, victim: &Inode, ctx: &dyn Context) -> SysResult<()> {
        let uattr = self.unstable_attr()?;
        if !uattr.perms.sticky {
            return Ok(());
        }
        let creds = ctx.credentials();
        if uattr.owner.uid == creds.effective_kuid {
            return Ok(());
        }
        let uattr = victim.unstable_attr()?;
        if uattr.owner.uid == creds.effective_kuid {
            return Ok(());
        }
        if victim.check_capability(&linux::Capability::fowner(), ctx) {
            Ok(())
        } else {
            err_libc!(libc::EPERM)
        }
    }
}
