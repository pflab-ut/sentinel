use std::sync::atomic::{AtomicI64, Ordering};

use mem::IoSequence;
use memmap::mmap_opts::MmapOpts;
use nix::fcntl;
use utils::{err_libc, SysError, SysResult};

use crate::{dentry::DentrySerializer, seek::SeekWhence, DirentRef};

use super::{attr::UnstableAttr, context::Context, FileOperations};

pub const FILE_MAX_OFFSET: i64 = i64::MAX;

#[derive(Debug)]
pub struct File {
    flags: FileFlags,
    file_operations: Box<dyn FileOperations>,
    offset: AtomicI64,
}

impl File {
    pub fn new(flags: FileFlags, file_operations: Box<dyn FileOperations>) -> Self {
        Self {
            flags,
            file_operations,
            offset: AtomicI64::new(0),
        }
    }

    #[inline]
    pub fn dirent(&self) -> DirentRef {
        self.file_operations.dirent()
    }

    #[inline]
    pub fn flags(&self) -> &FileFlags {
        &self.flags
    }

    #[inline]
    pub fn offset(&self) -> i64 {
        self.offset.load(Ordering::SeqCst)
    }

    pub fn set_flags(&mut self, new_flags: SettableFileFlags) {
        self.flags.direct = new_flags.direct;
        self.flags.non_blocking = new_flags.non_blocking;
        self.flags.append = new_flags.append;
        self.flags.async_ = new_flags.async_;
    }

    fn offset_for_append(&self, offset: &AtomicI64) -> SysResult<()> {
        let uattr = {
            let dirent = self.dirent();
            let dirent = dirent.borrow();
            dirent
                .inode()
                .unstable_attr()
                .map_err(|_| SysError::new(libc::EIO))?
        };
        offset.store(uattr.size, Ordering::SeqCst);
        Ok(())
    }

    pub fn writev(&self, src: &mut IoSequence, ctx: &dyn Context) -> SysResult<usize> {
        if self.flags.append {
            self.offset_for_append(&self.offset)?
        }
        let (limit, ok) = self.check_limit(self.offset.load(Ordering::Relaxed), ctx);
        if limit == 0 && ok {
            return Err(SysError::exceeds_file_size_limit());
        }
        if ok {
            src.take_first(limit as usize);
        }
        let offset = self.offset.load(Ordering::SeqCst);
        let n = self.file_operations.write(self.flags, src, offset, ctx)?;
        if !self.flags.non_seekable {
            self.offset.fetch_add(n as i64, Ordering::SeqCst);
        }
        Ok(n)
    }

    pub fn pwritev(
        &self,
        src: &mut IoSequence,
        offset: i64,
        ctx: &dyn Context,
    ) -> SysResult<usize> {
        if self.flags.append {
            self.offset_for_append(&self.offset)?;
        }

        let (limit, ok) = self.check_limit(offset, ctx);
        if ok && limit == 0 {
            return Err(SysError::exceeds_file_size_limit());
        } else if ok {
            src.take_first(limit as usize);
        }

        self.file_operations.write(self.flags, src, offset, ctx)
    }

    pub fn readv(&self, dst: &mut IoSequence, ctx: &dyn Context) -> SysResult<usize> {
        let n =
            self.file_operations
                .read(self.flags, dst, self.offset.load(Ordering::SeqCst), ctx)?;
        if n > 0 && !self.flags.non_seekable {
            self.offset.fetch_add(n as i64, Ordering::SeqCst);
        }
        Ok(n)
    }

    pub fn preadv(&self, dst: &mut IoSequence, offset: i64, ctx: &dyn Context) -> SysResult<usize> {
        self.file_operations.read(self.flags, dst, offset, ctx)
    }

    pub fn read_full(
        &self,
        dst: &mut IoSequence,
        offset: i64,
        ctx: &dyn Context,
    ) -> SysResult<usize> {
        let mut total = 0;
        while dst.num_bytes() > 0 {
            let n = self.preadv(dst, offset + total as i64, ctx)?;
            if n == 0 {
                break;
            }
            total += n;
            dst.drop_first(n);
        }
        Ok(total)
    }

    pub fn get_file_size(&self) -> SysResult<usize> {
        let dirent = self.dirent();
        let dirent = dirent.borrow();
        let uattr = dirent
            .inode()
            .unstable_attr()
            .map_err(|_| SysError::new(libc::EIO))?;
        Ok(uattr.size as usize)
    }

    fn check_limit(&self, offset: i64, ctx: &dyn Context) -> (i64, bool) {
        let attr = self.dirent().borrow().inode().stable_attr();
        if attr.is_regular() {
            let file_size_limit = ctx.limits().get_file_size().cur;
            if file_size_limit <= i64::MAX as u64 {
                if offset >= file_size_limit as i64 {
                    return (0, true);
                }
                return (file_size_limit as i64 - offset, true);
            }
        }
        (0, false)
    }

    pub fn configure_mmap(&mut self, opts: &mut MmapOpts) -> SysResult<()> {
        self.file_operations.configure_mmap(opts)
    }

    pub fn unstable_attr(&self) -> SysResult<UnstableAttr> {
        self.dirent().borrow().inode().unstable_attr()
    }

    pub fn flush(&self) -> SysResult<()> {
        self.file_operations.flush()
    }

    pub fn close(&self) -> SysResult<()> {
        self.file_operations.close()
    }

    pub fn ioctl(&self, regs: &libc::user_regs_struct, ctx: &dyn Context) -> SysResult<usize> {
        self.file_operations.ioctl(regs, ctx)
    }

    pub fn seek(&mut self, whence: SeekWhence, offset: i64) -> SysResult<i64> {
        let current = self.offset();
        let dirent = self.dirent();
        let dirent = dirent.borrow();
        let new_offset = self
            .file_operations
            .seek(dirent.inode(), whence, current, offset)?;
        self.offset.store(new_offset, Ordering::SeqCst);
        Ok(new_offset)
    }

    pub fn readdir(
        &mut self,
        serializer: &mut dyn DentrySerializer,
        ctx: &dyn Context,
    ) -> SysResult<()> {
        let offset = self.offset();
        match self.file_operations.readdir(offset, serializer, ctx) {
            Ok(offset) => {
                self.offset.store(offset, Ordering::SeqCst);
                Ok(())
            }
            Err(err) => {
                self.offset.store(err.value(), Ordering::SeqCst);
                err_libc!(err.code())
            }
        }
    }

    pub fn file_operations<T: 'static + FileOperations>(&self) -> Option<&T> {
        let fops = &self.file_operations;
        fops.as_any().downcast_ref::<T>()
    }

    pub fn file_operations_mut<T: 'static + FileOperations>(&mut self) -> Option<&mut T> {
        let fops = &mut self.file_operations;
        fops.as_any_mut().downcast_mut::<T>()
    }

    pub fn readiness(&self, mask: u64, ctx: &dyn Context) -> u64 {
        self.file_operations.readiness(mask, ctx)
    }
}

#[derive(Default, Clone, Copy, Debug)]
pub struct FileFlags {
    pub direct: bool,
    pub dsync: bool,
    pub sync: bool,
    pub append: bool,
    pub non_blocking: bool,
    pub read: bool,
    pub write: bool,
    pub pread: bool,
    pub pwrite: bool,
    pub directory: bool,
    pub async_: bool,
    pub large_file: bool,
    pub truncate: bool,
    pub non_seekable: bool,
}

impl FileFlags {
    pub fn from_linux_flags(mask: i32) -> Self {
        Self {
            direct: mask & libc::O_DIRECT != 0,
            dsync: mask & (libc::O_DSYNC | libc::O_SYNC) != 0,
            sync: mask & libc::O_SYNC != 0,
            non_blocking: mask & libc::O_NONBLOCK != 0,
            read: (mask & libc::O_ACCMODE) != libc::O_WRONLY,
            write: (mask & libc::O_ACCMODE) != libc::O_RDONLY,
            pread: false,
            pwrite: false,
            append: mask & libc::O_APPEND != 0,
            directory: mask & libc::O_DIRECTORY != 0,
            async_: mask & libc::O_ASYNC != 0,
            large_file: mask & libc::O_LARGEFILE != 0,
            truncate: mask & libc::O_TRUNC != 0,
            non_seekable: false,
        }
    }

    pub fn to_linux_flags(self) -> i32 {
        let mut mask = 0;
        if self.read && self.write {
            mask |= libc::O_RDWR;
        } else if self.read {
            mask |= libc::O_RDONLY;
        } else if self.write {
            mask |= libc::O_WRONLY;
        }
        if self.direct {
            mask |= libc::O_DIRECT;
        }
        if self.non_blocking {
            mask |= libc::O_NONBLOCK;
        }
        if self.dsync {
            mask |= libc::O_DSYNC;
        }
        if self.sync {
            mask |= libc::O_SYNC;
        }
        if self.append {
            mask |= libc::O_APPEND;
        }
        if self.directory {
            mask |= libc::O_DIRECTORY;
        }
        if self.async_ {
            mask |= libc::O_ASYNC;
        }
        if self.large_file {
            mask |= libc::O_LARGEFILE;
        }
        if self.truncate {
            mask |= libc::O_TRUNC;
        }
        mask
    }

    pub fn from_fd(fd: i32) -> SysResult<Self> {
        let flags = fcntl::fcntl(fd, fcntl::F_GETFL).map_err(|_| SysError::new(libc::EIO))?;
        let accmode = flags & libc::O_ACCMODE;
        Ok(Self {
            direct: flags & libc::O_DIRECTORY != 0,
            sync: flags & libc::O_SYNC != 0,
            non_blocking: flags & libc::O_NONBLOCK != 0,
            append: flags & libc::O_APPEND != 0,
            read: accmode == libc::O_RDONLY || accmode == libc::O_RDWR,
            write: accmode == libc::O_WRONLY || accmode == libc::O_RDWR,
            ..Self::default()
        })
    }

    pub fn as_settable(&self) -> SettableFileFlags {
        SettableFileFlags {
            direct: self.direct,
            non_blocking: self.non_blocking,
            append: self.append,
            async_: self.async_,
        }
    }
}

#[derive(Copy, Clone, Default)]
pub struct SettableFileFlags {
    pub direct: bool,
    pub non_blocking: bool,
    pub append: bool,
    pub async_: bool,
}
