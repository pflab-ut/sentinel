use std::{cell::RefCell, ffi::CString, path::PathBuf, rc::Rc};

use mem::{
    block_seq::BlockSeqView,
    io::{FromIoReader, FromIoWriter, Reader, Writer},
    IoSequence,
};
use memmap::mmap_opts::MmapOpts;
use net::get_poll_event_from_fd;
use nix::{
    sys::stat::{self, FchmodatFlags},
    unistd,
};
use utils::{bail_libc, SysError, SysResult};

use crate::{
    attr::{AttrMask, FilePermissions, StableAttr, UnstableAttr},
    context::Context,
    dentry::DentrySerializer,
    fsutils::{
        inode_cached::{CachingInodeOperations, CachingInodeOperationsOptions},
        seek_with_dir_cursor, FdReadWriter, SectionReader, SectionWriter,
    },
    inode,
    inode_operations::RenameUnderParents,
    mount::MountSource,
    seek::SeekWhence,
    DirentRef, File, FileFlags, FileOperations, InodeOperations, ReaddirError, ReaddirResult,
};

#[derive(Debug)]
pub struct RegularFile {
    caching_inode_ops: Rc<RefCell<CachingInodeOperations>>,
    file_object: Rc<RegularFileObject>,
}

impl InodeOperations for RegularFile {
    fn lookup(&mut self, _: &str, _: &dyn Context) -> SysResult<DirentRef> {
        bail_libc!(libc::ENOTDIR)
    }
    fn get_file(&self, dirent: DirentRef, mut flags: FileFlags) -> SysResult<File> {
        let sattr = {
            let dirent = dirent.borrow();
            dirent.inode().stable_attr()
        };
        if sattr.is_socket() {
            bail_libc!(libc::ENXIO);
        }
        if flags.write || flags.pwrite || flags.append {
            logger::warn!("modifying host::RegularFile is not allowed");
            bail_libc!(libc::EPERM);
        }
        flags.pread = true;
        Ok(File::new(
            flags,
            Box::new(RegularFileOperations {
                dirent,
                dir_cursor: String::new(),
            }),
        ))
    }
    fn unstable_attr(&self, msrc: &Rc<MountSource>, sattr: StableAttr) -> SysResult<UnstableAttr> {
        if !msrc.flags().force_page_cache || !sattr.is_file() {
            self.file_object.unstable_attr()
        } else {
            self.caching_inode_ops.borrow().unstable_attr(msrc, sattr)
        }
    }
    fn get_link(&self) -> SysResult<DirentRef> {
        bail_libc!(libc::ENOLINK)
    }
    fn read_link(&self) -> SysResult<String> {
        bail_libc!(libc::ENOLINK)
    }
    fn truncate(&mut self, _: i64, _: &dyn Context) -> SysResult<()> {
        logger::error!("modifying host::RegularFile is not allowed");
        bail_libc!(libc::EPERM);
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
    fn add_link(&self) {}
    fn drop_link(&self) {}
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl RegularFile {
    pub fn new(mut absolute_path: PathBuf) -> Self {
        let is_symlink = StableAttr::from_path(&absolute_path).unwrap().is_symlink();
        if is_symlink {
            absolute_path = nix::fcntl::readlink(&absolute_path).unwrap().into();
        }
        let uattr = UnstableAttr::from_path(&absolute_path).unwrap();
        let file_object = Rc::new(RegularFileObject::new(absolute_path));
        let iops = CachingInodeOperations::new(
            file_object.clone(),
            uattr,
            CachingInodeOperationsOptions::default(),
        );
        Self {
            caching_inode_ops: Rc::new(RefCell::new(iops)),
            file_object,
        }
    }

    fn read(&self, dst: &IoSequence, offset: i64) -> SysResult<usize> {
        let fd = self.file_object.fd().0;
        let r = FdReadWriter { fd };
        let r = SectionReader {
            reader: Box::new(r),
            off: offset as u64,
            limit: None,
        };
        dst.copy_out_from(&mut FromIoReader {
            reader: Box::new(r),
        })
    }
}

#[derive(Debug)]
struct RegularFileOperations {
    dirent: DirentRef,
    dir_cursor: String,
}

impl FileOperations for RegularFileOperations {
    fn dirent(&self) -> DirentRef {
        self.dirent.clone()
    }
    fn read(
        &self,
        _: FileFlags,
        dst: &mut IoSequence,
        offset: i64,
        _: &dyn Context,
    ) -> SysResult<usize> {
        let mut dirent = self.dirent.borrow_mut();
        dirent
            .inode_mut()
            .inode_operations_mut::<RegularFile>()
            .read(dst, offset)
    }
    fn write(&self, _: FileFlags, _: &mut IoSequence, _: i64, _: &dyn Context) -> SysResult<usize> {
        logger::error!("writing to host::RegularFile is not allowed");
        bail_libc!(libc::EPERM)
    }
    fn configure_mmap(&mut self, opts: &mut MmapOpts) -> SysResult<()> {
        if opts.offset + opts.length > i64::MAX as u64 {
            bail_libc!(libc::EOVERFLOW);
        }
        let dirent = self.dirent.borrow();
        let iops = dirent.inode().inode_operations::<RegularFile>();
        opts.mappable = Some(iops.caching_inode_ops.clone());
        Ok(())
    }
    fn flush(&self) -> SysResult<()> {
        Ok(())
    }
    fn close(&self) -> SysResult<()> {
        let dirent = self.dirent.borrow();
        let iops = dirent.inode().inode_operations::<RegularFile>();
        let file_object = &iops.file_object;
        file_object.close();
        Ok(())
    }
    fn ioctl(&self, regs: &libc::user_regs_struct, _: &dyn Context) -> SysResult<usize> {
        if regs.rsi == libc::FIONREAD {
            todo!();
        }
        bail_libc!(libc::ENOTTY)
    }
    fn seek(
        &mut self,
        inode: &inode::Inode,
        whence: SeekWhence,
        current_offset: i64,
        offset: i64,
    ) -> SysResult<i64> {
        seek_with_dir_cursor(
            inode,
            whence,
            current_offset,
            offset,
            Some(&mut self.dir_cursor),
        )
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
        let dirent = self.dirent.borrow();
        let inode = dirent.inode();
        let iops = inode.inode_operations::<RegularFile>();
        let (fd, new) = iops.file_object.fd();
        let res = get_poll_event_from_fd(fd, mask);
        if new {
            iops.file_object.close();
        }
        res
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[derive(Debug)]
pub struct RegularFileObject {
    pub fd: RefCell<i32>,
    absolute_path: PathBuf,
}

impl RegularFileObject {
    fn new(absolute_path: PathBuf) -> Self {
        Self {
            fd: RefCell::new(-1),
            absolute_path,
        }
    }

    fn unstable_attr(&self) -> SysResult<UnstableAttr> {
        UnstableAttr::from_path(&self.absolute_path).map_err(SysError::from_nix_errno)
    }

    pub fn fd(&self) -> (i32, bool) {
        let mut fd = self.fd.borrow_mut();
        if *fd > 0 {
            (*fd, false)
        } else {
            let cstr = CString::new(self.absolute_path.to_str().unwrap().as_bytes()).unwrap();
            let f = unsafe { libc::open(cstr.as_ptr(), libc::O_RDONLY) };
            *fd = f;
            (f, true)
        }
    }

    pub fn close(&self) {
        let mut fd = self.fd.borrow_mut();
        if *fd > 0 {
            unsafe { libc::close(*fd) };
            *fd = -1;
        }
    }

    pub fn read_to_blocks_at(&self, dsts: BlockSeqView, off: u64) -> SysResult<usize> {
        let reader = FdReadWriter { fd: self.fd().0 };
        let reader = SectionReader {
            reader: Box::new(reader),
            off,
            limit: None,
        };
        let mut reader = FromIoReader {
            reader: Box::new(reader),
        };
        reader.read_to_blocks(dsts)
    }

    pub fn write_from_blocks_at(&self, srcs: BlockSeqView, off: u64) -> SysResult<usize> {
        let writer = FdReadWriter { fd: self.fd().0 };
        let mut writer = SectionWriter {
            writer: Box::new(writer),
            off,
            limit: None,
        };
        let mut writer = FromIoWriter {
            writer: &mut writer,
        };
        writer.write_from_blocks(srcs)
    }

    pub fn set_masked_attributes(&self, mask: AttrMask, attr: UnstableAttr) -> SysResult<()> {
        if mask.is_empty() {
            return Ok(());
        }
        if mask.uid || mask.gid {
            bail_libc!(libc::EPERM);
        }
        if mask.perms {
            let mode = stat::Mode::from_bits(attr.perms.as_linux_mode()).unwrap();
            stat::fchmodat(
                None,
                &self.absolute_path,
                mode,
                FchmodatFlags::FollowSymlink,
            )
            .map_err(SysError::from_nix_errno)?;
        }
        if mask.size {
            unistd::truncate(&self.absolute_path, attr.size).map_err(SysError::from_nix_errno)?;
        }
        if mask.access_time || mask.modification_time {
            todo!("setTimespamp");
        }
        Ok(())
    }
}
