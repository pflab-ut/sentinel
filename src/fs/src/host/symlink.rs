use std::{path::PathBuf, rc::Rc};

use mem::IoSequence;
use memmap::mmap_opts::MmapOpts;
use utils::{bail_libc, SysError, SysResult};

use crate::{
    attr, context::Context, dentry::DentrySerializer, fsutils::inode::InodeSimpleAttributes, inode,
    inode_operations::RenameUnderParents, mount::MountSource, seek::SeekWhence, DirentRef, File,
    FileFlags, FileOperations, InodeOperations, ReaddirError, ReaddirResult,
};

#[derive(Debug)]
pub struct Symlink {
    simple_attr: InodeSimpleAttributes,
    target: PathBuf,
}

impl InodeOperations for Symlink {
    fn lookup(&mut self, _: &str, _: &dyn Context) -> SysResult<DirentRef> {
        bail_libc!(libc::ENOTDIR)
    }
    fn get_file(&self, dirent: DirentRef, flags: FileFlags) -> SysResult<File> {
        Ok(File::new(flags, Box::new(SymlinkFileOperations { dirent })))
    }
    fn unstable_attr(
        &self,
        msrc: &Rc<MountSource>,
        sattr: attr::StableAttr,
    ) -> SysResult<attr::UnstableAttr> {
        let mut uattr = self.simple_attr.unstable_attr(msrc, sattr)?;
        uattr.size = self.target.as_path().to_str().unwrap().len() as i64;
        uattr.usage = uattr.size;
        Ok(uattr)
    }
    fn get_link(&self) -> SysResult<DirentRef> {
        Err(SysError::resolve_via_readlink())
    }
    fn read_link(&self) -> SysResult<String> {
        // FIXME
        Ok(self.target.to_str().unwrap().to_string())
    }
    fn truncate(&mut self, _: i64, _: &dyn Context) -> SysResult<()> {
        bail_libc!(libc::EINVAL)
    }
    fn create(
        &mut self,
        _: attr::UnstableAttr,
        _: Rc<MountSource>,
        _: &str,
        _: FileFlags,
        _: attr::FilePermissions,
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
    fn add_link(&self) {}
    fn drop_link(&self) {}
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl Symlink {
    pub fn new(simple_attr: InodeSimpleAttributes, target: PathBuf) -> Self {
        Self {
            simple_attr,
            target,
        }
    }
}

#[derive(Clone, Debug)]
struct SymlinkFileOperations {
    dirent: DirentRef,
}

impl FileOperations for SymlinkFileOperations {
    fn dirent(&self) -> DirentRef {
        self.dirent.clone()
    }
    fn read(&self, _: FileFlags, _: &mut IoSequence, _: i64, _: &dyn Context) -> SysResult<usize> {
        bail_libc!(libc::EINVAL)
    }
    fn write(&self, _: FileFlags, _: &mut IoSequence, _: i64, _: &dyn Context) -> SysResult<usize> {
        bail_libc!(libc::EINVAL)
    }
    fn configure_mmap(&mut self, _: &mut MmapOpts) -> SysResult<()> {
        bail_libc!(libc::ENODEV)
    }
    fn flush(&self) -> SysResult<()> {
        Ok(())
    }
    fn close(&self) -> SysResult<()> {
        Ok(())
    }
    fn ioctl(&self, _: &libc::user_regs_struct, _: &dyn Context) -> SysResult<usize> {
        bail_libc!(libc::ENOTTY)
    }
    fn seek(&mut self, _: &inode::Inode, _: SeekWhence, _: i64, _: i64) -> SysResult<i64> {
        bail_libc!(libc::EINVAL)
    }
    fn readdir(
        &mut self,
        _: i64,
        _: &mut dyn DentrySerializer,
        _: &dyn Context,
    ) -> ReaddirResult<i64> {
        Err(ReaddirError::new(0, libc::ENOTDIR))
    }
    fn readiness(&self, mask: u64, _: &dyn Context) -> u64 {
        mask
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
