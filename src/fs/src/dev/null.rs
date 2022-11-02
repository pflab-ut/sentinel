use std::rc::Rc;

use linux::FileMode;
use utils::{err_libc, SysError, SysResult};

use crate::{
    attr::{FileOwner, FilePermissions, StableAttr, UnstableAttr},
    fsutils::{inode::InodeSimpleAttributes, seek_with_dir_cursor},
    inode_operations::RenameUnderParents,
    mount::MountSource,
    Context, File, FileOperations, InodeOperations,
};

#[derive(Debug)]
pub struct NullDevice {
    simple_attr: InodeSimpleAttributes,
}

impl NullDevice {
    pub fn new(owner: FileOwner, mode: FileMode, ctx: &dyn Context) -> Self {
        let simple_attr = InodeSimpleAttributes::new(
            owner,
            FilePermissions::from_mode(mode),
            linux::TMPFS_MAGIC,
            &|| ctx.now(),
        );
        Self { simple_attr }
    }
}

impl InodeOperations for NullDevice {
    fn lookup(&mut self, _: &str, _: &dyn Context) -> SysResult<crate::DirentRef> {
        err_libc!(libc::ENOTDIR)
    }
    fn get_file(&self, dirent: crate::DirentRef, mut flags: crate::FileFlags) -> SysResult<File> {
        flags.pread = true;
        flags.pwrite = true;
        Ok(File::new(
            flags,
            Box::new(NullDeviceFileOperations { dirent }),
        ))
    }
    fn unstable_attr(
        &self,
        msrc: &Rc<MountSource>,
        sattr: StableAttr,
    ) -> SysResult<crate::attr::UnstableAttr> {
        self.simple_attr.unstable_attr(msrc, sattr)
    }
    fn get_link(&self) -> SysResult<crate::DirentRef> {
        err_libc!(libc::ENOLINK)
    }
    fn read_link(&self) -> SysResult<String> {
        err_libc!(libc::ENOLINK)
    }
    fn truncate(&mut self, _: i64, _: &dyn Context) -> SysResult<()> {
        Ok(())
    }
    fn create(
        &mut self,
        _: UnstableAttr,
        _: Rc<MountSource>,
        _: &str,
        _: crate::FileFlags,
        _: FilePermissions,
        _: &dyn Context,
    ) -> SysResult<File> {
        err_libc!(libc::ENOTDIR)
    }
    fn rename(
        &self,
        _: RenameUnderParents<&mut crate::inode::Inode>,
        _: &str,
        _: String,
        _: bool,
        _: &dyn Context,
    ) -> SysResult<()> {
        err_libc!(libc::EINVAL)
    }
    fn add_link(&self) {
        self.simple_attr.add_link()
    }
    fn drop_link(&self) {
        self.simple_attr.drop_link()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[derive(Debug)]
pub struct NullDeviceFileOperations {
    pub dirent: crate::DirentRef,
}

impl FileOperations for NullDeviceFileOperations {
    fn dirent(&self) -> crate::DirentRef {
        self.dirent.clone()
    }
    fn read(
        &self,
        _: crate::FileFlags,
        _: &mut mem::IoSequence,
        _: i64,
        _: &dyn Context,
    ) -> SysResult<usize> {
        Ok(0)
    }
    fn write(
        &self,
        _: crate::FileFlags,
        src: &mut mem::IoSequence,
        _: i64,
        _: &dyn Context,
    ) -> SysResult<usize> {
        Ok(src.num_bytes() as usize)
    }
    fn configure_mmap(&mut self, _: &mut memmap::mmap_opts::MmapOpts) -> SysResult<()> {
        err_libc!(libc::ENODEV)
    }
    fn flush(&self) -> SysResult<()> {
        Ok(())
    }
    fn close(&self) -> SysResult<()> {
        Ok(())
    }
    fn ioctl(&self, _: &libc::user_regs_struct, _: &dyn Context) -> SysResult<usize> {
        err_libc!(libc::ENOTTY)
    }
    fn seek(
        &mut self,
        inode: &crate::inode::Inode,
        whence: crate::seek::SeekWhence,
        current_offset: i64,
        offset: i64,
    ) -> SysResult<i64> {
        seek_with_dir_cursor(inode, whence, current_offset, offset, None)
    }
    fn readdir(
        &mut self,
        _: i64,
        _: &mut dyn crate::dentry::DentrySerializer,
        _: &dyn Context,
    ) -> crate::ReaddirResult<i64> {
        Err(crate::ReaddirError::new(0, libc::ENOTDIR))
    }
    fn readiness(&self, _: u64, _: &dyn Context) -> u64 {
        unimplemented!()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
