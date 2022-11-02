use utils::{bail_libc, err_libc, SysError, SysResult};

use crate::{io, AddrRangeSeqView};

use super::{block::Block, block_seq::BlockSeq, Addr, AddrRange, IoOpts};

pub struct BytesIo {
    data: *mut u8,
    len: usize,
}

impl BytesIo {
    pub fn new(buf: &mut [u8]) -> Self {
        Self {
            data: buf.as_mut_ptr(),
            len: buf.len(),
        }
    }

    pub fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.data as *const _, self.len) }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.data, self.len) }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn range_check(&self, addr: Addr, length: i32) -> (i32, SysResult<()>) {
        match length {
            0 => (0, Ok(())),
            length if length < 0 => (0, err_libc!(libc::EINVAL)),
            length => {
                let max = Addr(self.len as u64);
                if addr >= max {
                    (0, err_libc!(libc::EFAULT))
                } else {
                    match addr.add_length(length as u64) {
                        None => ((max - addr).0 as i32, Err(SysError::new(libc::EFAULT))),
                        Some(end) => {
                            if end > max {
                                ((max - addr).0 as i32, Err(SysError::new(libc::EFAULT)))
                            } else {
                                (length, Ok(()))
                            }
                        }
                    }
                }
            }
        }
    }

    fn block_from_addr_ranges(&self, ar: AddrRange) -> (Block, SysResult<()>) {
        let (n, res) = self.range_check(Addr(ar.start), ar.len() as i32);
        if n == 0 {
            (Block::default(), res)
        } else {
            (
                Block::from_slice(
                    &self.bytes()[ar.start as usize..ar.start as usize + n as usize],
                    false,
                ),
                res,
            )
        }
    }

    fn blocks_from_addr_ranges(&self, mut ars: AddrRangeSeqView) -> (BlockSeq, SysResult<()>) {
        match ars.num_ranges() {
            0 => (BlockSeq::default(), Ok(())),
            1 => {
                let (block, res) = self.block_from_addr_ranges(ars.head());
                (BlockSeq::from_block(block), res)
            }
            _ => {
                let mut blocks = Vec::with_capacity(ars.num_ranges() as usize);
                while !ars.is_empty() {
                    let (block, res) = self.block_from_addr_ranges(ars.head());
                    if !block.is_empty() {
                        blocks.push(block);
                    }
                    if res.is_err() {
                        return (BlockSeq::from_blocks(blocks), res);
                    }
                    ars = ars.tail();
                }
                (BlockSeq::from_blocks(blocks), Ok(()))
            }
        }
    }
}

impl io::Io for BytesIo {
    fn copy_out(&mut self, addr: Addr, src: &[u8], _: &IoOpts) -> SysResult<usize> {
        let (rng_n, rng_res) = self.range_check(addr, src.len() as i32);
        if rng_n == 0 {
            rng_res?;
            Ok(0)
        } else {
            let count = std::cmp::min(rng_n as usize, self.len() - addr.0 as usize);
            unsafe {
                std::ptr::copy_nonoverlapping(src.as_ptr(), self.data.add(addr.0 as usize), count)
            };
            rng_res?;
            Ok(count)
        }
    }

    fn copy_out_from(
        &mut self,
        ars: AddrRangeSeqView,
        src: &mut dyn io::Reader,
        _: &IoOpts,
    ) -> SysResult<usize> {
        let (dsts, rng_res) = self.blocks_from_addr_ranges(ars);
        let res = src.read_to_blocks(dsts.as_view());
        rng_res?;
        res
    }

    fn zero_out(&mut self, addr: Addr, to_zero: i64, _: &IoOpts) -> SysResult<usize> {
        if to_zero > i32::MAX as i64 {
            bail_libc!(libc::EINVAL);
        }
        let (rng_n, rng_res) = self.range_check(addr, to_zero as i32);
        if rng_n == 0 {
            return Ok(0);
        }
        self.bytes_mut()[addr.0 as usize..addr.0 as usize + rng_n as usize].fill(0);
        rng_res?;
        Ok(rng_n as usize)
    }

    fn copy_in(&mut self, addr: Addr, dst: &mut [u8], _: &IoOpts) -> SysResult<usize> {
        let dst_len = dst.len();
        let (rng_n, rng_res) = self.range_check(addr, dst_len as i32);
        if rng_n == 0 {
            rng_res?;
            Ok(0)
        } else {
            let count = std::cmp::min(rng_n as usize, self.len() - addr.0 as usize);
            unsafe {
                std::ptr::copy_nonoverlapping(
                    self.data.add(addr.0 as usize),
                    dst.as_mut_ptr(),
                    count,
                )
            };
            rng_res?;
            Ok(count)
        }
    }

    fn copy_in_to(
        &mut self,
        ars: AddrRangeSeqView,
        dst: &mut dyn io::Writer,
        _: &IoOpts,
    ) -> SysResult<usize> {
        let (srcs, rng_res) = self.blocks_from_addr_ranges(ars);
        let res = dst.write_from_blocks(srcs.as_view());
        rng_res?;
        res
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, BufWriter};

    use crate::io::Io;

    use super::*;

    fn new_bytes_io_string(s: &mut [u8]) -> BytesIo {
        BytesIo::new(s)
    }

    #[test]
    fn bytes_io_copy_out_ok() {
        let mut data = String::from("ABCDE");
        let data = unsafe { data.as_bytes_mut() };
        let mut b = new_bytes_io_string(data);
        let src = "foo";
        let n = b.copy_out(Addr(1), src.as_bytes(), &IoOpts::default());
        assert_eq!(n, Ok(3));
        assert_eq!(b.bytes(), "AfooE".as_bytes());
    }

    #[test]
    fn bytes_io_copy_out_err() {
        let mut data = String::from("ABC");
        let data = unsafe { data.as_bytes_mut() };
        let mut b = new_bytes_io_string(data);
        let src = "foo";
        let res = b.copy_out(Addr(1), src.as_bytes(), &IoOpts::default());
        assert_eq!(res, Err(SysError::new(libc::EFAULT)));
        assert_eq!(b.bytes(), "Afo".as_bytes());
    }

    #[test]
    fn bytes_io_copy_in_ok() {
        let mut data = String::from("AfooE");
        let data = unsafe { data.as_bytes_mut() };
        let mut b = new_bytes_io_string(data);
        let mut dst = vec![0; 3];
        let n = b.copy_in(Addr(1), &mut dst, &IoOpts::default());
        assert_eq!(n, Ok(3));
        assert_eq!(dst, "foo".as_bytes().to_vec());
    }

    #[test]
    fn bytes_io_copy_in_err() {
        let mut data = String::from("Afo");
        let data = unsafe { data.as_bytes_mut() };
        let mut b = new_bytes_io_string(data);
        let mut dst = vec![0; 3];
        let res = b.copy_in(Addr(1), &mut dst, &IoOpts::default());
        assert_eq!(res, Err(SysError::new(libc::EFAULT)));
        assert_eq!(dst, "fo\x00".as_bytes().to_vec());
    }

    #[test]
    fn bytes_io_zero_out_ok() {
        let mut data = String::from("ABCD");
        let data = unsafe { data.as_bytes_mut() };
        let mut b = new_bytes_io_string(data);
        let n = b.zero_out(Addr(1), 2, &IoOpts::default());
        assert_eq!(n, Ok(2));
        assert_eq!(b.bytes(), "A\x00\x00D".as_bytes());
    }

    #[test]
    fn bytes_io_zero_out_err() {
        let mut data = String::from("ABC");
        let data = unsafe { data.as_bytes_mut() };
        let mut b = new_bytes_io_string(data);
        let res = b.zero_out(Addr(1), 3, &IoOpts::default());
        assert_eq!(res, Err(SysError::new(libc::EFAULT)));
        assert_eq!(b.bytes(), "A\x00\x00".as_bytes());
    }

    #[test]
    fn bytes_io_copy_out_from_ok() {
        let mut data = String::from("ABCDEFGH");
        let data = unsafe { data.as_bytes_mut() };
        let mut b = new_bytes_io_string(data);
        let mut r = io::FromIoReader {
            reader: Box::new(BufReader::new("barfoo".as_bytes())),
        };
        let n = b.copy_out_from(
            AddrRangeSeqView::from_slice(&[
                AddrRange { start: 4, end: 7 },
                AddrRange { start: 1, end: 4 },
            ]),
            &mut r,
            &IoOpts::default(),
        );
        assert_eq!(n, Ok(6));
        assert_eq!(b.bytes(), "AfoobarH".as_bytes());
    }

    #[test]
    fn bytes_io_copy_out_from_err() {
        let mut data = String::from("ABCDE");
        let data = unsafe { data.as_bytes_mut() };
        let mut b = new_bytes_io_string(data);
        let mut r = io::FromIoReader {
            reader: Box::new(BufReader::new("foobar".as_bytes())),
        };
        let res = b.copy_out_from(
            AddrRangeSeqView::from_slice(&[
                AddrRange { start: 1, end: 4 },
                AddrRange { start: 4, end: 7 },
            ]),
            &mut r,
            &IoOpts::default(),
        );
        assert_eq!(res, Err(SysError::new(libc::EFAULT)));
        assert_eq!(b.bytes(), "Afoob".as_bytes());
    }

    #[test]
    fn bytes_io_copy_in_to_ok() {
        let mut data = String::from("AfoobarH");
        let data = unsafe { data.as_bytes_mut() };
        let mut b = new_bytes_io_string(data);
        let mut writer = BufWriter::new(Vec::new());
        let mut w = io::FromIoWriter {
            writer: &mut writer,
        };
        let n = b.copy_in_to(
            AddrRangeSeqView::from_slice(&[
                AddrRange { start: 4, end: 7 },
                AddrRange { start: 1, end: 4 },
            ]),
            &mut w,
            &IoOpts::default(),
        );
        assert_eq!(n, Ok(6));
        assert_eq!(writer.buffer(), "barfoo".as_bytes());
    }

    #[test]
    fn bytes_io_copy_in_to_err() {
        let mut data = String::from("Afoob");
        let data = unsafe { data.as_bytes_mut() };
        let mut b = new_bytes_io_string(data);
        let mut writer = BufWriter::new(Vec::new());
        let mut w = io::FromIoWriter {
            writer: &mut writer,
        };
        let res = b.copy_in_to(
            AddrRangeSeqView::from_slice(&[
                AddrRange { start: 1, end: 4 },
                AddrRange { start: 4, end: 7 },
            ]),
            &mut w,
            &IoOpts::default(),
        );
        assert_eq!(res, Err(SysError::new(libc::EFAULT)));
        assert_eq!(writer.buffer(), "foob".as_bytes());
    }
}
