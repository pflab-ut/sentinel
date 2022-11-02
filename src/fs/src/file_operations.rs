use std::any::Any;

use mem::IoSequence;
use memmap::mmap_opts::MmapOpts;
use utils::SysResult;

use crate::{
    dentry::DentrySerializer, inode, seek::SeekWhence, DirentRef, FileFlags, ReaddirResult,
};

use super::Context;

pub trait FileOperations: std::fmt::Debug {
    fn dirent(&self) -> DirentRef;
    fn read(
        &self,
        flags: FileFlags,
        dst: &mut IoSequence,
        offset: i64,
        ctx: &dyn Context,
    ) -> SysResult<usize>;
    fn write(
        &self,
        flags: FileFlags,
        src: &mut IoSequence,
        offset: i64,
        ctx: &dyn Context,
    ) -> SysResult<usize>;
    fn configure_mmap(&mut self, opts: &mut MmapOpts) -> SysResult<()>;
    fn flush(&self) -> SysResult<()>;
    fn close(&self) -> SysResult<()>;
    fn ioctl(&self, regs: &libc::user_regs_struct, ctx: &dyn Context) -> SysResult<usize>;
    fn seek(
        &mut self,
        inode: &inode::Inode,
        whence: SeekWhence,
        current_offset: i64,
        offset: i64,
    ) -> SysResult<i64>;
    fn readdir(
        &mut self,
        offset: i64,
        serializer: &mut dyn DentrySerializer,
        ctx: &dyn Context,
    ) -> ReaddirResult<i64>;
    fn readiness(&self, mask: u64, ctx: &dyn Context) -> u64;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}
