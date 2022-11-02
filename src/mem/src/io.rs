use utils::{SysError, SysResult};

use crate::{block_seq::BlockSeqView, AddrRangeSeqView};

use super::{
    block::{copy, Block},
    block_seq::{copy_seq, BlockSeq},
    Addr, IoOpts,
};

pub trait Reader {
    fn read_to_blocks(&mut self, dsts: BlockSeqView) -> SysResult<usize>;
}

pub trait Writer {
    fn write_from_blocks(&mut self, srcs: BlockSeqView) -> SysResult<usize>;
}

pub fn read_full_to_blocks(mut r: impl Reader, mut dsts: BlockSeq) -> SysResult<usize> {
    let mut done = 0;
    while !dsts.is_empty() {
        let n = r.read_to_blocks(dsts.as_view())?;
        if n == 0 {
            break;
        }
        done += n;
        dsts.drop_first64(n as u64);
    }
    Ok(done)
}

pub fn write_full_to_blocks(mut w: impl Writer, mut srcs: BlockSeq) -> SysResult<usize> {
    let mut done = 0;
    while !srcs.is_empty() {
        let n = w.write_from_blocks(srcs.as_view())?;
        if n == 0 {
            break;
        }
        done += n;
        srcs.drop_first64(n as u64);
    }
    Ok(done)
}

pub struct BlockSeqReader {
    pub src: BlockSeq,
}

impl Reader for BlockSeqReader {
    fn read_to_blocks(&mut self, dsts: BlockSeqView) -> SysResult<usize> {
        let n = copy_seq(dsts, self.src.as_view())?;
        self.src.drop_first64(n as u64);
        Ok(n)
    }
}

pub const COPY_MAP_MIN_BYTES: u64 = 32 << 10;
pub const RW_MAP_MIN_BYTES: u64 = 512;

pub trait Io {
    fn copy_out(&mut self, addr: Addr, src: &[u8], opts: &IoOpts) -> SysResult<usize>;
    fn copy_in(&mut self, addr: Addr, dst: &mut [u8], opts: &IoOpts) -> SysResult<usize>;
    fn zero_out(&mut self, addr: Addr, to_zero: i64, opts: &IoOpts) -> SysResult<usize>;
    fn copy_out_from(
        &mut self,
        ars: AddrRangeSeqView,
        src: &mut dyn Reader,
        opts: &IoOpts,
    ) -> SysResult<usize>;
    fn copy_in_to(
        &mut self,
        ars: AddrRangeSeqView,
        dst: &mut dyn Writer,
        opts: &IoOpts,
    ) -> SysResult<usize>;
}

pub struct FromIoReader {
    pub reader: Box<dyn std::io::Read>,
}

impl Reader for FromIoReader {
    fn read_to_blocks(&mut self, mut dsts: BlockSeqView) -> SysResult<usize> {
        let mut done = 0;
        while !dsts.is_empty() {
            let mut dst = dsts.head();
            let n = self.read_to_block(&mut dst)?;
            done += n;
            if n != dst.len() as usize {
                return Ok(done);
            }
            dsts = dsts.tail();
        }
        Ok(done)
    }
}

impl FromIoReader {
    fn read_to_block(&mut self, dst: &mut Block) -> SysResult<usize> {
        if !dst.need_safe_copy() {
            let slice = unsafe { dst.as_slice_mut() };
            self.reader.read(slice).map_err(SysError::from_io_error)
        } else {
            let mut buf = vec![0; dst.len() as usize];
            let rn = self
                .reader
                .read(&mut buf)
                .map_err(SysError::from_io_error)?;
            copy(dst, &Block::from_slice(&buf[..rn as usize], false)).map(|n| n as usize)
        }
    }
}

pub struct FromIoWriter<'a> {
    pub writer: &'a mut dyn std::io::Write,
}

impl<'a> Writer for FromIoWriter<'a> {
    fn write_from_blocks(&mut self, mut srcs: BlockSeqView) -> SysResult<usize> {
        let mut done = 0;
        while !srcs.is_empty() {
            let src = srcs.head();
            let n = self.write_from_block(&src)?;
            done += n;
            if n != src.len() as usize {
                return Ok(done);
            }
            srcs = srcs.tail();
        }
        Ok(done)
    }
}

impl<'a> FromIoWriter<'a> {
    fn write_from_block(&mut self, src: &Block) -> SysResult<usize> {
        if !src.need_safe_copy() {
            let slice = unsafe { src.as_slice() };
            self.writer.write(slice).map_err(SysError::from_io_error)
        } else {
            let buf = vec![0; src.len() as usize];
            let mut block = Block::from_slice(&buf, false);
            let bufn = copy(&mut block, src)?;
            self.writer
                .write(&buf[..bufn])
                .map_err(SysError::from_io_error)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, BufWriter};

    use super::*;

    fn build_blocks(slices: &Vec<Vec<u8>>) -> Vec<Block> {
        let mut blocks = Vec::with_capacity(slices.len());
        for s in slices {
            blocks.push(Block::from_slice(s, false));
        }
        blocks
    }

    #[test]
    fn from_io_reader_full_read() {
        let mut r = FromIoReader {
            reader: Box::new(BufReader::new("foobar".as_bytes())),
        };
        let data = vec![vec![0; 3], vec![0; 3]];
        let dsts = build_blocks(&data);
        let seqs = BlockSeqView::from_slice(&dsts);
        let n = r.read_to_blocks(seqs);
        assert_eq!(n, Ok(6));
        for (i, want) in ["foo".as_bytes(), "bar".as_bytes()].iter().enumerate() {
            unsafe {
                assert_eq!(&dsts[i].as_slice(), want);
            }
        }
    }

    #[test]
    fn from_io_reader_partial_read() {
        let mut r = FromIoReader {
            reader: Box::new(BufReader::new("foob".as_bytes())),
        };
        let data = vec![vec![0; 3], vec![0; 3]];
        let dsts = build_blocks(&data);
        let seqs = BlockSeqView::from_slice(&dsts);
        let n = r.read_to_blocks(seqs);
        assert_eq!(n, Ok(4));
        for (i, want) in ["foo".as_bytes(), "b\x00\x00".as_bytes()]
            .iter()
            .enumerate()
        {
            unsafe {
                assert_eq!(&dsts[i].as_slice(), want);
            }
        }
    }

    struct SingleByteReader {
        reader: Box<dyn std::io::Read>,
    }

    impl std::io::Read for SingleByteReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if buf.is_empty() {
                self.reader.read(buf)
            } else {
                self.reader.read(&mut buf[..1])
            }
        }
    }

    #[test]
    fn single_byte_reader() {
        let byte_reader = SingleByteReader {
            reader: Box::new(BufReader::new("foobar".as_bytes())),
        };
        let mut r = FromIoReader {
            reader: Box::new(byte_reader),
        };
        let data = vec![vec![0; 3], vec![0; 3]];
        let dsts = build_blocks(&data);
        let seqs = BlockSeqView::from_slice(&dsts);
        let n = r.read_to_blocks(seqs);
        assert_eq!(n, Ok(1));
        for (i, want) in ["f\x00\x00".as_bytes(), "\x00\x00\x00".as_bytes()]
            .iter()
            .enumerate()
        {
            unsafe {
                assert_eq!(&dsts[i].as_slice(), want);
            }
        }
    }

    #[test]
    fn single_byte_reader_read_full_to_blocks() {
        let byte_reader = SingleByteReader {
            reader: Box::new(BufReader::new("foobar".as_bytes())),
        };
        let r = FromIoReader {
            reader: Box::new(byte_reader),
        };
        let data = vec![vec![0; 3], vec![0; 3]];
        let dsts = build_blocks(&data);
        let seqs = BlockSeq::from_blocks(dsts.clone());
        let n = read_full_to_blocks(r, seqs);
        assert_eq!(n, Ok(6));
        for (i, want) in ["foo".as_bytes(), "bar".as_bytes()].iter().enumerate() {
            unsafe {
                assert_eq!(&dsts[i].as_slice(), want);
            }
        }
    }

    #[test]
    fn from_io_writer_full_write() {
        let data = vec!["foo".as_bytes().to_vec(), "bar".as_bytes().to_vec()];
        let srcs = build_blocks(&data);
        let seq = BlockSeqView::from_slice(&srcs);
        let mut buf_writer = BufWriter::new(Vec::new());
        let mut w = FromIoWriter {
            writer: &mut buf_writer,
        };
        let n = w.write_from_blocks(seq);
        assert_eq!(n, Ok(6));
        assert_eq!(buf_writer.buffer(), "foobar".as_bytes());
    }

    struct LimitedWriter<'a> {
        writer: &'a mut dyn std::io::Write,
        done: usize,
        limit: usize,
    }

    impl<'a> std::io::Write for LimitedWriter<'a> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let count = std::cmp::min(buf.len(), self.limit - self.done);
            let n = self.writer.write(&buf[..count])?;
            self.done += n;
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            unimplemented!("Not needed for this test.");
        }
    }

    #[test]
    fn from_io_writer_partial_write() {
        let data = vec!["foo".as_bytes().to_vec(), "bar".as_bytes().to_vec()];
        let srcs = build_blocks(&data);
        let seq = BlockSeqView::from_slice(&srcs);
        let mut buf_writer = BufWriter::new(Vec::new());
        let mut writer = LimitedWriter {
            writer: &mut buf_writer,
            done: 0,
            limit: 4,
        };
        let mut w = FromIoWriter {
            writer: &mut writer,
        };
        let n = w.write_from_blocks(seq);
        assert_eq!(n, Ok(4));
        assert_eq!(buf_writer.buffer(), "foob".as_bytes());
    }

    struct SingleByteWriter<'a> {
        writer: &'a mut dyn std::io::Write,
    }

    impl<'a> std::io::Write for SingleByteWriter<'a> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if buf.is_empty() {
                self.writer.write(buf)
            } else {
                self.writer.write(&buf[..1])
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            unimplemented!("Not needed for this test.");
        }
    }

    #[test]
    fn single_byte_writer() {
        let data = vec!["foo".as_bytes().to_vec(), "bar".as_bytes().to_vec()];
        let srcs = build_blocks(&data);
        let mut buf_writer = BufWriter::new(Vec::new());
        let mut single_byte_writer = SingleByteWriter {
            writer: &mut buf_writer,
        };
        let mut w = FromIoWriter {
            writer: &mut single_byte_writer,
        };
        let seq = BlockSeqView::from_slice(&srcs);
        let n = w.write_from_blocks(seq);
        assert_eq!(n, Ok(1));
        assert_eq!(buf_writer.buffer(), "f".as_bytes().to_vec());
    }

    #[test]
    fn from_io_writer_write_full_to_blocks() {
        let data = vec!["foo".as_bytes().to_vec(), "bar".as_bytes().to_vec()];
        let srcs = build_blocks(&data);
        let seq = BlockSeq::from_blocks(srcs);
        let mut buf_writer = BufWriter::new(Vec::new());
        let mut single_byte_writer = SingleByteWriter {
            writer: &mut buf_writer,
        };
        let w = FromIoWriter {
            writer: &mut single_byte_writer,
        };
        let n = write_full_to_blocks(w, seq);
        assert_eq!(n, Ok(6));
        assert_eq!(buf_writer.buffer(), "foobar".as_bytes());
    }
}
