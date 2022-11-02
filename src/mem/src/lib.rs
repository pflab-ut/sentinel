mod access_type;
mod addr;
mod addr_range_seq;
pub mod block;
pub mod block_seq;
pub mod bytes_io;
mod context;
pub mod io;

pub use access_type::AccessType;
pub use addr::*;
pub use addr_range_seq::*;
pub use context::Context;

use std::{cell::RefCell, rc::Rc};
use utils::{bail_libc, SysError, SysResult};

use bytes_io::BytesIo;

pub const PAGE_SHIFT: i32 = 12;
pub const PAGE_SIZE: i32 = 1 << PAGE_SHIFT;
pub const HUGE_PAGE_SHIFT: i32 = 21;
pub const HUGE_PAGE_SIZE: u64 = 1u64 << HUGE_PAGE_SHIFT;

#[derive(Default, Clone, Copy)]
pub struct IoOpts {
    pub ignore_permissions: bool,
}

pub struct IoSequence {
    pub io: Rc<RefCell<dyn io::Io>>,
    pub addrs: AddrRangeSeq,
    pub opts: IoOpts,
}

impl IoSequence {
    pub fn drop_first(&mut self, n: usize) {
        self.addrs.drop_first(n);
    }

    pub fn take_first(&mut self, n: usize) {
        self.addrs.truncate_to_first(n);
    }

    pub fn num_bytes(&self) -> usize {
        self.addrs.num_bytes()
    }

    pub fn copy_in(&self, dst: &mut [u8]) -> SysResult<usize> {
        copy_in_vec(&self.io, self.addrs.as_view(), dst, &self.opts)
    }

    pub fn copy_in_to(&self, dst: &mut dyn io::Writer) -> SysResult<usize> {
        self.io
            .as_ref()
            .borrow_mut()
            .copy_in_to(self.addrs.as_view(), dst, &self.opts)
    }

    pub fn copy_out(&self, src: &[u8]) -> SysResult<usize> {
        copy_out_vec(&self.io, self.addrs.as_view(), src, &self.opts)
    }

    pub fn copy_out_from(&self, src: &mut dyn io::Reader) -> SysResult<usize> {
        self.io
            .as_ref()
            .borrow_mut()
            .copy_out_from(self.addrs.as_view(), src, &self.opts)
    }

    pub fn bytes_sequence(buf: &mut [u8]) -> Self {
        Self {
            io: Rc::new(RefCell::new(BytesIo::new(buf))),
            addrs: AddrRangeSeq::from(AddrRange {
                start: 0,
                end: buf.len() as u64,
            }),
            opts: IoOpts::default(),
        }
    }
}

impl std::io::Read for IoSequence {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self
            .copy_in(buf)
            .map_err(|e| std::io::Error::from_raw_os_error(e.code()))?;
        self.drop_first(n);
        Ok(n)
    }
}

impl std::io::Write for IoSequence {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self
            .copy_out(buf)
            .map_err(|e| std::io::Error::from_raw_os_error(e.code()))?;
        self.drop_first(n);
        if n < buf.len() {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "write beyond end of IoSequence.",
            ))
        } else {
            Ok(n)
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        unimplemented!()
    }
}

pub struct IoReadWriter {
    pub addr: Addr,
    pub io: Rc<RefCell<dyn io::Io>>,
    pub opts: IoOpts,
}

impl std::io::Write for IoReadWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self
            .io
            .borrow_mut()
            .copy_out(self.addr, buf, &self.opts)
            .map_err(|e| std::io::Error::from_raw_os_error(e.code()))?;
        self.addr = self.addr.add_length(n as u64).unwrap_or(Addr(!(0u64)));
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        unimplemented!()
    }
}

const COPY_STRING_MAX_INIT_BUF_LEN: usize = 256;
const COPY_STRING_INCREMENT: usize = 64;

// copy_string_in copies a NUL-terminated string of unknown length from the memory mapped at addr
// in uio and returns it as a string (not including the terminating NUL)
pub fn copy_string_in(
    uio: &Rc<RefCell<impl io::Io>>,
    mut addr: Addr,
    max_len: usize,
    opts: &IoOpts,
) -> SysResult<String> {
    let init_len = std::cmp::min(max_len, COPY_STRING_MAX_INIT_BUF_LEN);
    let mut buf = vec![0; init_len as usize];
    let mut done = 0;
    while done < max_len {
        let mut read_len = std::cmp::min(max_len - done, COPY_STRING_INCREMENT);
        let mut end = addr
            .add_length(read_len as u64)
            .ok_or_else(|| SysError::new(libc::EFAULT))?;
        if addr.round_down() != end.round_down() {
            end = end.round_down();
            read_len = (end.0 - addr.0) as usize;
        }
        if done + read_len > buf.len() {
            let new_buf_len = std::cmp::min(buf.len() * 2, max_len);
            buf.extend_from_slice(&vec![0; new_buf_len - buf.len()]);
        }
        let n = uio
            .as_ref()
            .borrow_mut()
            .copy_in(addr, &mut buf[done..done + read_len], opts)?;

        if let Some(index) = &buf[done..done + n].iter().position(|x| *x == 0) {
            let s = std::str::from_utf8(&buf[..done + index])
                .expect("failed to convert bytes to str")
                .to_string();
            return Ok(s);
        }

        done += n;
        addr = end;
    }
    bail_libc!(libc::ENAMETOOLONG)
}

fn copy_out_vec(
    uio: &Rc<RefCell<dyn io::Io>>,
    mut ars: AddrRangeSeqView,
    src: &[u8],
    opts: &IoOpts,
) -> SysResult<usize> {
    let mut done = 0;
    while !ars.is_empty() && done < src.len() {
        let ar = ars.head();
        let cplen = std::cmp::min(src.len() - done, ar.len() as usize);
        let n = uio
            .borrow_mut()
            .copy_out(Addr(ar.start), &src[done..done + cplen], opts)?;
        done += n;
        ars = ars.drop_first(n);
    }
    Ok(done)
}

fn copy_in_vec(
    uio: &Rc<RefCell<dyn io::Io>>,
    mut ars: AddrRangeSeqView,
    dst: &mut [u8],
    opts: &IoOpts,
) -> SysResult<usize> {
    let mut done = 0;
    while !ars.is_empty() && done < dst.len() {
        let ar = ars.head();
        let cplen = std::cmp::min(dst.len() - done, ar.len() as usize);
        let n = uio
            .borrow_mut()
            .copy_in(Addr(ar.start), &mut dst[done..done + cplen], opts)?;
        done += n;
        ars = ars.drop_first(n);
    }
    Ok(done)
}
