use std::{
    any::Any,
    cell::RefCell,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use dev::Device;
use fs::{
    attr::{FilePermissions, InodeType, PermMask, StableAttr, UnstableAttr},
    dentry::DentrySerializer,
    fsutils::inode::InodeSimpleAttributes,
    mount::MountSource,
    seek::SeekWhence,
    Context, DirentRef, FileFlags, FileOperations, InodeOperations, ReaddirError, ReaddirResult,
    RenameUnderParents,
};
use mem::{
    block::Block,
    block_seq::{copy_seq, BlockSeq},
    Addr, IoSequence, PAGE_SIZE,
};
use memmap::mmap_opts::MmapOpts;
use once_cell::sync::Lazy;
use time::Context as TimeContext;
use utils::{bail_libc, err_libc, SysError, SysResult};

use crate::context;

#[derive(Debug)]
struct Pipe {
    buf: Vec<u8>,
    buf_block_seq: BlockSeq,
    offset: usize,
    size: usize,
    max: usize,
    has_reader: AtomicBool,
    has_writer: AtomicBool,
}

const MIN_PIPE_SIZE: usize = PAGE_SIZE as usize;
const MAX_PIPE_SIZE: usize = 1048576;
const ATOMIC_IO_BYTES: usize = 4096;
pub const DEFAULT_PIPE_SIZE: usize = 16 * PAGE_SIZE as usize;

impl Pipe {
    fn new(max: usize) -> Self {
        let max = std::cmp::min(max, MAX_PIPE_SIZE);
        let max = std::cmp::max(max, MIN_PIPE_SIZE);
        Pipe {
            buf: Vec::new(),
            buf_block_seq: BlockSeq::default(),
            offset: 0,
            size: 0,
            max,
            has_reader: AtomicBool::new(false),
            has_writer: AtomicBool::new(false),
        }
    }

    fn write_impl<F: FnMut(BlockSeq) -> SysResult<usize>>(
        &mut self,
        mut count: usize,
        mut f: F,
    ) -> SysResult<usize> {
        if !self.has_writer.load(Ordering::SeqCst) {
            bail_libc!(libc::ESPIPE);
        }
        let available = self.max - self.size;
        if available == 0 {
            bail_libc!(libc::EWOULDBLOCK);
        }
        let mut short = false;
        if count > available {
            if count <= ATOMIC_IO_BYTES {
                bail_libc!(libc::EWOULDBLOCK);
            }
            count = available;
            short = true;
        }
        let new_len = self.size + count;
        let old_cap = self.buf.len();
        if new_len > old_cap {
            let mut new_cap = if old_cap == 0 { 8 } else { old_cap * 2 };
            while new_len > new_cap {
                new_cap *= 2;
            }
            let new_cap = std::cmp::min(new_cap, self.max);
            let new_buf = vec![0; new_cap];
            let dst = BlockSeq::from_block(Block::from_slice(&new_buf, false));
            let src = self
                .buf_block_seq
                .cut_first(self.offset as u64)
                .take_first64(self.size as u64);
            copy_seq(dst.as_view(), src.as_view())?;
            let block = Block::from_slice(&new_buf, false);
            self.buf = new_buf;
            self.buf_block_seq = BlockSeq::from_blocks(vec![block, block]);
            self.offset = 0;
        }
        let mut write_offset = self.offset + self.size;
        if write_offset >= self.buf.len() {
            write_offset -= self.buf.len();
        }
        let bs = self
            .buf_block_seq
            .cut_first(write_offset as u64)
            .take_first64(count as u64);
        let done = f(bs)?;
        self.size += done;
        if done < count {
            Ok(done)
        } else if short {
            err_libc!(libc::EWOULDBLOCK)
        } else {
            Ok(done)
        }
    }

    fn read_impl<F: FnMut(BlockSeq) -> SysResult<usize>>(
        &mut self,
        count: usize,
        f: F,
        remove: bool,
    ) -> SysResult<usize> {
        let n = self.peek(count, f)?;
        if n > 0 && remove {
            self.consume(n);
        }
        Ok(n)
    }

    fn peek<F: FnMut(BlockSeq) -> SysResult<usize>>(
        &self,
        mut count: usize,
        mut f: F,
    ) -> SysResult<usize> {
        if count == 0 {
            return Ok(0);
        }
        if count > self.size {
            if self.size == 0 {
                if !self.has_writer.load(Ordering::SeqCst) {
                    bail_libc!(libc::EOF);
                } else {
                    bail_libc!(libc::EWOULDBLOCK);
                }
            }
            count = self.size;
        }
        let bs = self
            .buf_block_seq
            .cut_first(self.offset as u64)
            .take_first64(count as u64);
        f(bs)
    }

    fn consume(&mut self, n: usize) {
        self.offset += n;
        let max = self.buf.len();
        if self.offset >= max {
            self.offset -= max;
        }
        self.size -= n;
    }
}

static PIPE_DEVICE: Lazy<Arc<Mutex<Device>>> = Lazy::new(dev::Device::new_anonymous_device);

// Pipe is shared between files, so make it a shared pointer.
#[derive(Debug, Clone)]
pub struct PipeRef {
    pipe: Rc<RefCell<Pipe>>,
    dirent: Option<DirentRef>,
}

impl PipeRef {
    pub fn new(max: usize) -> Self {
        let pipe = Rc::new(RefCell::new(Pipe::new(max)));
        Self { pipe, dirent: None }
    }

    fn open(&self, mut flags: FileFlags) -> fs::File {
        flags.non_seekable = true;
        if flags.read && flags.write {
            self.pipe.borrow().has_reader.store(true, Ordering::SeqCst);
            self.pipe.borrow().has_writer.store(true, Ordering::SeqCst);
            fs::File::new(flags, Box::new(self.clone()))
        } else if flags.read {
            self.pipe.borrow().has_reader.store(true, Ordering::SeqCst);
            fs::File::new(flags, Box::new(self.clone()))
        } else if flags.write {
            self.pipe.borrow().has_writer.store(true, Ordering::SeqCst);
            fs::File::new(flags, Box::new(self.clone()))
        } else {
            panic!("invalid pipe flags")
        }
    }

    pub fn connect(&mut self) -> (fs::File, fs::File) {
        let perms = FilePermissions {
            user: PermMask {
                read: true,
                write: true,
                execute: false,
            },
            ..FilePermissions::default()
        };
        let ctx = context::context();
        let iops = PipeInodeOperations {
            simple_attrs: InodeSimpleAttributes::new(
                ctx.file_owner(),
                perms,
                linux::PIPEFS_MAGIC,
                &|| ctx.now(),
            ),
        };
        let dev = PIPE_DEVICE.lock().unwrap();
        let inode_id = dev.next_ino();
        let sattr = StableAttr {
            typ: InodeType::Pipe,
            device_id: dev.device_id(),
            inode_id,
            block_size: ATOMIC_IO_BYTES as i64,
            device_file_major: 0,
            device_file_minor: 0,
        };
        let ms = MountSource::new_pseudo();
        let inode = fs::inode::Inode::new(Box::new(iops), Rc::new(ms), sattr);
        let d = fs::Dirent::new(inode, format!("pipe:[{}]", inode_id));
        self.dirent = Some(d);
        let r = self.open(FileFlags {
            read: true,
            ..FileFlags::default()
        });
        let w = self.open(FileFlags {
            write: true,
            ..FileFlags::default()
        });
        (r, w)
    }
}

impl FileOperations for PipeRef {
    fn dirent(&self) -> DirentRef {
        self.dirent.as_ref().unwrap().clone()
    }
    fn read(
        &self,
        _: fs::FileFlags,
        dst: &mut IoSequence,
        _: i64,
        _: &dyn fs::Context,
    ) -> SysResult<usize> {
        self.pipe.borrow_mut().read_impl(
            dst.num_bytes() as usize,
            |mut srcs| {
                let mut done = 0;
                while !srcs.is_empty() {
                    let src = srcs.head();
                    let n = dst.copy_out(unsafe { src.as_slice() })?;
                    done += n;
                    dst.drop_first(n);
                    srcs = srcs.tail();
                }
                Ok(done)
            },
            true,
        )
    }
    fn write(
        &self,
        _: fs::FileFlags,
        src: &mut IoSequence,
        _: i64,
        _: &dyn fs::Context,
    ) -> SysResult<usize> {
        self.pipe
            .borrow_mut()
            .write_impl(src.num_bytes() as usize, |mut dsts| {
                let mut done = 0;
                while !dsts.is_empty() {
                    let mut dst = dsts.head();
                    let n = src.copy_in(unsafe { dst.as_slice_mut() })?;
                    done += n;
                    src.drop_first(n);
                    dsts = dsts.tail();
                }
                Ok(done)
            })
    }
    fn configure_mmap(&mut self, _: &mut MmapOpts) -> SysResult<()> {
        err_libc!(libc::ENODEV)
    }
    fn flush(&self) -> SysResult<()> {
        Ok(())
    }
    fn close(&self) -> SysResult<()> {
        Ok(())
    }
    fn ioctl(&self, regs: &libc::user_regs_struct, _: &dyn fs::Context) -> SysResult<usize> {
        match regs.rsi as u64 {
            libc::FIONREAD => {
                let v = self.pipe.borrow().size;
                let v = std::cmp::min(v, i32::MAX as usize) as i32;
                let dst = Addr(regs.rdx);
                let ctx = context::context();
                let task = ctx.task();
                task.copy_out_bytes(dst, &v.to_le_bytes())?;
                Ok(0)
            }
            _ => err_libc!(libc::ENOTTY),
        }
    }
    fn seek(&mut self, _: &fs::inode::Inode, _: SeekWhence, _: i64, _: i64) -> SysResult<i64> {
        err_libc!(libc::ESPIPE)
    }
    fn readdir(
        &mut self,
        _: i64,
        _: &mut dyn DentrySerializer,
        _: &dyn fs::Context,
    ) -> ReaddirResult<i64> {
        Err(ReaddirError::new(0, libc::ENOTDIR))
    }
    fn readiness(&self, _: u64, _: &dyn fs::Context) -> u64 {
        todo!()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[derive(Debug)]
pub struct PipeInodeOperations {
    simple_attrs: InodeSimpleAttributes,
}

impl InodeOperations for PipeInodeOperations {
    fn lookup(&mut self, _: &str, _: &dyn fs::Context) -> SysResult<DirentRef> {
        err_libc!(libc::ENOTDIR)
    }
    fn get_file(&self, _: DirentRef, _: FileFlags) -> SysResult<fs::File> {
        todo!()
    }
    fn unstable_attr(&self, msrc: &Rc<MountSource>, sattr: StableAttr) -> SysResult<UnstableAttr> {
        self.simple_attrs.unstable_attr(msrc, sattr)
    }
    fn get_link(&self) -> SysResult<DirentRef> {
        err_libc!(libc::ENOLINK)
    }
    fn read_link(&self) -> SysResult<String> {
        err_libc!(libc::ENOLINK)
    }
    fn truncate(&mut self, _: i64, _: &dyn fs::Context) -> SysResult<()> {
        Ok(())
    }
    fn create(
        &mut self,
        _: UnstableAttr,
        _: Rc<MountSource>,
        _: &str,
        _: FileFlags,
        _: FilePermissions,
        _: &dyn Context,
    ) -> SysResult<fs::File> {
        err_libc!(libc::ENOTDIR)
    }
    fn rename(
        &self,
        _: RenameUnderParents<&mut fs::inode::Inode>,
        _: &str,
        _: String,
        _: bool,
        _: &dyn Context,
    ) -> SysResult<()> {
        err_libc!(libc::EINVAL)
    }
    fn add_link(&self) {
        self.simple_attrs.add_link()
    }
    fn drop_link(&self) {
        self.simple_attrs.drop_link()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
