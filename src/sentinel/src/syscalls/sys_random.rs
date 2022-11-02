use mem::{
    io::{FromIoReader, Io},
    Addr, AddrRangeSeqView, IoOpts,
};
use rand::RngCore;
use utils::{bail_libc, err_libc, SysError};

use crate::context;

const GRND_NONBLOCK: u64 = 0x1;
const GRND_RANDOM: u64 = 0x2;

// getrandom implements linux syscall getrandom(2)
pub fn getrandom(regs: &libc::user_regs_struct) -> super::Result {
    let buf = Addr(regs.rdi);
    let buflen = regs.rsi;
    let flags = regs.rdx;

    if flags & !(GRND_NONBLOCK | GRND_RANDOM) != 0 {
        bail_libc!(libc::EINVAL);
    }

    let buflen = std::cmp::min(buflen, i32::MAX as u64);
    let ar = buf
        .to_range(buflen)
        .ok_or_else(|| SysError::new(libc::EFAULT))?;

    let min = std::cmp::min(256, buflen as i32);
    let ctx = context::context();
    let mm = ctx.memory_manager();
    let mut mm = mm.borrow_mut();
    let ar = &[ar];
    match mm.copy_out_from(
        AddrRangeSeqView::from_slice(ar),
        &mut FromIoReader {
            reader: Box::new(RandReader),
        },
        &IoOpts {
            ignore_permissions: false,
        },
    ) {
        Ok(n) if n >= min as usize => Ok(n),
        Ok(_) => err_libc!(libc::EAGAIN),
        Err(err) => Err(err),
    }
}

struct RandReader;

impl std::io::Read for RandReader {
    // TODO: naive implementation
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut rng = rand::thread_rng();
        rng.fill_bytes(buf);
        Ok(buf.len())
    }
}
