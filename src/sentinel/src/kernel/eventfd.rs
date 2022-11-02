use fs::{inode::Inode, FileFlags, FileOperations};
use time::Time;
use utils::{err_libc, SysError, SysResult};

pub fn new_eventfd<F: Fn() -> Time>(timer: F) -> fs::File {
    let inode = Inode::new_anon(timer);
    let dirent = fs::Dirent::new(inode, "anon_inode:[eventfd]".to_string());
    fs::File::new(
        FileFlags {
            read: true,
            write: true,
            ..FileFlags::default()
        },
        Box::new(EventFileOperations { dirent }),
    )
}

#[derive(Debug)]
pub struct EventFileOperations {
    dirent: fs::DirentRef,
}

impl FileOperations for EventFileOperations {
    fn dirent(&self) -> fs::DirentRef {
        self.dirent.clone()
    }
    fn read(
        &self,
        _: fs::FileFlags,
        _: &mut mem::IoSequence,
        _: i64,
        _: &dyn fs::Context,
    ) -> SysResult<usize> {
        todo!()
    }
    fn write(
        &self,
        _: fs::FileFlags,
        _: &mut mem::IoSequence,
        _: i64,
        _: &dyn fs::Context,
    ) -> SysResult<usize> {
        todo!()
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
    fn ioctl(&self, _: &libc::user_regs_struct, _: &dyn fs::Context) -> SysResult<usize> {
        err_libc!(libc::ENOTTY)
    }
    fn seek(
        &mut self,
        _: &fs::inode::Inode,
        _: fs::seek::SeekWhence,
        _: i64,
        _: i64,
    ) -> SysResult<i64> {
        err_libc!(libc::ESPIPE)
    }
    fn readdir(
        &mut self,
        _: i64,
        _: &mut dyn fs::dentry::DentrySerializer,
        _: &dyn fs::Context,
    ) -> fs::ReaddirResult<i64> {
        Err(fs::ReaddirError::new(0, libc::ENOTDIR))
    }
    fn readiness(&self, _: u64, _: &dyn fs::Context) -> u64 {
        todo!()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
