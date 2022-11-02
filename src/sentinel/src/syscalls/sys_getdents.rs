use std::io::BufWriter;

use fs::dentry::{DentAttr, DentrySerializer};
use mem::{Addr, IoOpts, IoReadWriter};
use utils::{err_libc, SysError, SysResult};

use crate::context;

// getdents implements linux syscall getdents(2)
pub fn getdents(regs: &libc::user_regs_struct) -> super::Result {
    let fd = regs.rdi as i32;
    let addr = Addr(regs.rsi);
    let size = regs.rdx as i32;

    let min_size = smallest_dirent() as i32;
    if size < min_size {
        err_libc!(libc::EINVAL)
    } else {
        getdents_impl(fd, addr, size, LinuxDirentType::Getdents)
    }
}

// getdents64 implements linux syscall getdents(2)
pub fn getdents64(regs: &libc::user_regs_struct) -> super::Result {
    let fd = regs.rdi as i32;
    let addr = Addr(regs.rsi);
    let size = regs.rdx as i32;

    let min_size = smallest_dirent64() as i32;
    if size < min_size {
        err_libc!(libc::EINVAL)
    } else {
        getdents_impl(fd, addr, size, LinuxDirentType::Getdents64)
    }
}

fn getdents_impl(fd: i32, addr: Addr, size: i32, how: LinuxDirentType) -> SysResult<usize> {
    let ctx = &*context::context();
    let mut task = ctx.task_mut();
    let dir = task
        .get_file(fd)
        .ok_or_else(|| SysError::new(libc::EBADF))?;

    let w = IoReadWriter {
        io: task.memory_manager().clone(),
        addr,
        opts: IoOpts::default(),
    };

    let mut ds = DirentSerializer {
        how,
        writer: Box::new(w),
        offset: 0,
        written: 0,
        size,
    };

    let mut dir = dir.borrow_mut();
    match dir.readdir(&mut ds, ctx) {
        Ok(()) => Ok(ds.written_bytes()),
        Err(err) if err.code() == libc::EOF => Ok(0),
        Err(err) => Err(err),
    }
}

const WIDTH: usize = 8;

const fn smallest_dirent() -> u32 {
    std::mem::size_of::<libc::dirent>() as u32 + WIDTH as u32 + 1
}

const fn smallest_dirent64() -> u32 {
    std::mem::size_of::<libc::dirent64>() as u32 + WIDTH as u32
}

struct LinuxDirent {
    is_dirent64: bool,
    ino: libc::ino_t,
    off: libc::off_t,
    typ: libc::c_uchar,
    name: Vec<u8>,
}

impl LinuxDirent {
    fn new(name: &str, attr: DentAttr, offset: i64, how: LinuxDirentType) -> Self {
        let name = name.as_bytes().to_vec();
        LinuxDirent {
            is_dirent64: how == LinuxDirentType::Getdents64,
            ino: attr.inode_id,
            off: offset,
            typ: attr.typ.as_dirent_type(),
            name,
        }
    }

    fn serialize(&self, w: &mut dyn std::io::Write) -> std::io::Result<usize> {
        let name_len = self.name.len();
        if self.is_dirent64 {
            let size = 8 + 8 + 2 + 1 + 1 + name_len;
            let size = (size + 7) & !7;
            let mut buf = Vec::with_capacity(size);
            buf.extend_from_slice(&self.ino.to_le_bytes());
            buf.extend_from_slice(&(self.off as u64).to_le_bytes());
            buf.extend_from_slice(&(size as u16).to_le_bytes());
            buf.extend_from_slice(&self.typ.to_le_bytes());
            buf.extend_from_slice(&self.name);
            let padding = size - buf.len();
            buf.extend_from_slice(&vec![0; padding]);
            w.write(&buf)
        } else {
            todo!()
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum LinuxDirentType {
    Getdents,
    Getdents64,
}

struct DirentSerializer {
    how: LinuxDirentType,
    writer: Box<dyn std::io::Write>,
    offset: i64,
    written: i32,
    size: i32,
}

impl DentrySerializer for DirentSerializer {
    fn copy_out(&mut self, name: &str, attr: DentAttr) -> std::io::Result<()> {
        self.offset += 1;

        let d = LinuxDirent::new(name, attr, self.offset, self.how);
        let mut w = {
            let buf = Vec::new();
            BufWriter::new(buf)
        };
        let n = d.serialize(&mut w).map_err(|e| {
            self.offset -= 1;
            e
        })?;
        if n > (self.size - self.written) as usize {
            self.offset -= 1;
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "libc::EOF",
            ));
        }
        self.writer.write(w.buffer()).map_err(|e| {
            self.offset -= 1;
            e
        })?;
        self.written += n as i32;
        Ok(())
    }

    fn written_bytes(&self) -> usize {
        self.written as usize
    }
}
