mod host_file_mapper;
pub mod inode;
pub mod inode_cached;

pub use host_file_mapper::*;

use mem::{block_seq::zero_seq, AccessType, Addr};
use memmap::file::MemmapFile;
use segment::{Seg, Set, SetOperations};
use utils::{bail_libc, FileRange, Range, SysError, SysResult};

use crate::{attr::InodeType, file::FILE_MAX_OFFSET, inode::Inode, seek::SeekWhence};

use super::{utils::io_err_from_nix_errno, Context};

pub type FileRangeSet = Set<u64, u64>;

pub struct FileRangeSetOperations;
impl SetOperations for FileRangeSetOperations {
    type K = u64;
    type V = u64;
    fn merge(
        &self,
        r1: Range<Self::K>,
        v1: &Self::V,
        _r2: Range<Self::K>,
        v2: &Self::V,
    ) -> Option<Self::V> {
        if *v1 + r1.len() != *v2 {
            None
        } else {
            Some(*v1)
        }
    }

    fn split(&self, r: Range<Self::K>, v: &Self::V, split: Self::K) -> (Self::V, Self::V) {
        (*v, *v + split - r.start)
    }
}

pub trait SetU64Operations {
    fn truncate(&mut self, end: u64, ctx: &dyn Context);
    fn file_range_of(&self, seg: &Seg<u64>, r: Range<u64>) -> Range<u64>;
}

impl SetU64Operations for FileRangeSet {
    fn truncate(&mut self, end: u64, ctx: &dyn Context) {
        let mut mf = ctx.memory_file_provider().memory_file_write_lock();
        if let Some(pg_end_addr) = Addr(end).round_up() {
            let pg_end = pg_end_addr.0;
            self.split_at(pg_end);
            let mut seg = self.lower_bound_segment(pg_end);
            while let Some(seg_inner) = seg {
                let removed = self.remove(seg_inner.range());
                seg = self.next_segment_of_gap(&removed);
            }
            if end == pg_end {
                return;
            }
        }

        let seg = self.find_segment(end);
        if let Some(seg) = seg {
            let mut fr = self.file_range_of(&seg, seg.range());
            fr.start += end - seg.start();
            let ims = mf
                .map_internal(fr, AccessType::write())
                .unwrap_or_else(|e| panic!("failed to map {:?}: {:?}", fr, e));
            zero_seq(ims.as_view()).unwrap_or_else(|e| panic!("Zeroing {:?} failed: {:?}", fr, e));
        }
    }

    fn file_range_of(&self, seg: &Seg<u64>, r: Range<u64>) -> Range<u64> {
        let frstart = self.inner_map().get(&seg.range()).unwrap() + (r.start - seg.start());
        FileRange {
            start: frstart,
            end: frstart + r.len(),
        }
    }
}

pub struct FdReadWriter {
    pub fd: i32,
}

impl std::io::Read for FdReadWriter {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        nix::unistd::read(self.fd, buf).map_err(|_| io_err_from_nix_errno())
    }
}

impl std::io::Write for FdReadWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut remaining = buf.len();
        while remaining > 0 {
            let woff = buf.len() - remaining;
            let n = nix::unistd::write(self.fd, &buf[woff..])?;
            if n == 0 {
                break;
            }
            remaining -= n;
        }
        Ok(buf.len() - remaining)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        unimplemented!()
    }
}

impl std::io::Seek for FdReadWriter {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        let res = match pos {
            std::io::SeekFrom::Start(p) => {
                nix::unistd::lseek(self.fd, p as i64, nix::unistd::Whence::SeekSet)
            }
            std::io::SeekFrom::End(p) => {
                nix::unistd::lseek(self.fd, p, nix::unistd::Whence::SeekEnd)
            }
            std::io::SeekFrom::Current(p) => {
                nix::unistd::lseek(self.fd, p, nix::unistd::Whence::SeekCur)
            }
        };
        match res {
            Ok(n) => Ok(n as u64),
            Err(_) => Err(io_err_from_nix_errno()),
        }
    }
}

pub struct SectionReader<T: std::io::Read + std::io::Seek> {
    pub reader: Box<T>,
    pub off: u64,
    pub limit: Option<u64>,
}

impl<T: std::io::Read + std::io::Seek> std::io::Read for SectionReader<T> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut max = buf.len();
        if let Some(limit) = self.limit {
            if limit - self.off < buf.len() as u64 {
                max = (limit - self.off) as usize;
            }
        }
        self.reader.seek(std::io::SeekFrom::Start(self.off))?;
        let n = self.reader.read(&mut buf[..max])?;
        self.off += n as u64;
        self.reader.rewind()?;
        Ok(n)
    }
}

pub struct SectionWriter<T: std::io::Write + std::io::Seek> {
    pub writer: Box<T>,
    pub off: u64,
    pub limit: Option<u64>,
}

impl<T: std::io::Write + std::io::Seek> std::io::Write for SectionWriter<T> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut max = buf.len();
        if let Some(limit) = self.limit {
            if limit - self.off < buf.len() as u64 {
                max = (limit - self.off) as usize;
            }
        }
        self.writer.seek(std::io::SeekFrom::Start(self.off))?;
        let n = self.writer.write(&buf[..max])?;
        self.off += n as u64;
        self.writer.rewind()?;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        unimplemented!();
    }
}

pub fn seek_with_dir_cursor(
    inode: &Inode,
    whence: SeekWhence,
    current_offset: i64,
    offset: i64,
    dir_cursor: Option<&mut String>,
) -> SysResult<i64> {
    let sattr = inode.stable_attr();
    if sattr.is_pipe() || sattr.is_socket() {
        bail_libc!(libc::ESPIPE);
    }
    if sattr.is_char_device() {
        return Ok(0);
    }
    match whence {
        SeekWhence::Set => match sattr.typ {
            InodeType::RegularFile | InodeType::SpecialFile | InodeType::BlockDevice => {
                if offset < 0 {
                    bail_libc!(libc::EINVAL);
                } else {
                    Ok(offset)
                }
            }
            InodeType::Directory | InodeType::SpecialDirectory => {
                if offset != 0 {
                    bail_libc!(libc::EINVAL);
                }
                if let Some(s) = dir_cursor {
                    *s = String::from("");
                }
                Ok(0)
            }
            _ => bail_libc!(libc::EINVAL),
        },
        SeekWhence::Current => match sattr.typ {
            InodeType::RegularFile | InodeType::SpecialFile | InodeType::BlockDevice => {
                if current_offset + offset < 0 {
                    bail_libc!(libc::EINVAL);
                }
                Ok(current_offset + offset)
            }
            InodeType::Directory | InodeType::SpecialDirectory => {
                if offset != 0 {
                    bail_libc!(libc::EINVAL);
                }
                Ok(current_offset)
            }
            _ => bail_libc!(libc::EINVAL),
        },
        SeekWhence::End => match sattr.typ {
            InodeType::RegularFile | InodeType::BlockDevice => {
                let uattr = inode.unstable_attr()?;
                let sz = uattr.size;
                if sz + offset < 0 {
                    bail_libc!(libc::EINVAL);
                }
                Ok(sz + offset)
            }
            InodeType::SpecialDirectory => {
                if offset != 0 {
                    bail_libc!(libc::EINVAL);
                }
                Ok(FILE_MAX_OFFSET)
            }
            _ => bail_libc!(libc::EINVAL),
        },
    }
}
