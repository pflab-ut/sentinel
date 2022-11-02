use std::collections::HashMap;

use mem::{block::Block, block_seq::BlockSeq, HUGE_PAGE_SHIFT};
use utils::{bail_libc, FileRange, SysError, SysResult};

const CHUNK_SHIFT: u32 = HUGE_PAGE_SHIFT as u32;
const CHUNK_SIZE: u64 = 1 << CHUNK_SHIFT;
const CHUNK_MASK: u64 = CHUNK_SIZE - 1;

#[derive(Clone, Copy, Debug)]
struct Mapping {
    addr: usize,
    writable: bool,
}

#[derive(Debug, Default)]
pub struct HostFileMapper {
    mappings: HashMap<u64, Mapping>,
}

impl HostFileMapper {
    pub fn map_internal(&mut self, fr: FileRange, fd: i32, write: bool) -> SysResult<BlockSeq> {
        let chunks = ((fr.end + CHUNK_MASK) >> CHUNK_SHIFT) - (fr.start >> CHUNK_SHIFT);
        if chunks == 1 {
            let mut seq = BlockSeq::default();
            self.for_each_mapping_block(fr, fd, write, &mut |b| {
                seq = BlockSeq::from_block(b);
            })?;
            Ok(seq)
        } else {
            let mut blocks = Vec::with_capacity(chunks as usize);
            self.for_each_mapping_block(fr, fd, write, &mut |b| {
                blocks.push(b);
            })?;
            Ok(BlockSeq::from_blocks(blocks))
        }
    }

    fn for_each_mapping_block<F: FnMut(Block)>(
        &mut self,
        fr: FileRange,
        fd: i32,
        write: bool,
        mut f: F,
    ) -> SysResult<()> {
        let prot = if write {
            libc::PROT_READ | libc::PROT_WRITE
        } else {
            libc::PROT_READ
        };
        let mut chunk_start = fr.start & !CHUNK_MASK;
        loop {
            let m_addr = match self.mappings.get(&chunk_start) {
                None => {
                    let addr = unsafe {
                        libc::mmap(
                            std::ptr::null_mut(),
                            CHUNK_SIZE as usize,
                            prot,
                            libc::MAP_SHARED,
                            fd,
                            chunk_start as i64,
                        )
                    };
                    if addr == libc::MAP_FAILED {
                        logger::error!("mmap failed for_each_mapping_block in HostFileMapper");
                        bail_libc!(libc::EINVAL);
                    }
                    let addr = addr as usize;
                    let m = Mapping {
                        addr,
                        writable: write,
                    };
                    self.mappings.insert(chunk_start, m);
                    addr
                }
                Some(m) => {
                    if write && !m.writable {
                        let addr = unsafe {
                            libc::mmap(
                                m.addr as *mut _,
                                CHUNK_SIZE as usize,
                                prot,
                                libc::MAP_SHARED | libc::MAP_FIXED,
                                fd,
                                chunk_start as i64,
                            )
                        };
                        if addr == libc::MAP_FAILED {
                            logger::error!("mmap failed for_each_mapping_block in HostFileMapper");
                            bail_libc!(libc::EINVAL);
                        }
                        let addr = addr as usize;
                        let m = Mapping {
                            addr,
                            writable: write,
                        };
                        self.mappings.insert(chunk_start, m);
                        addr
                    } else {
                        m.addr
                    }
                }
            };
            let start_off = if chunk_start < fr.start {
                fr.start - chunk_start
            } else {
                0
            };
            let end_off = if chunk_start + CHUNK_SIZE > fr.end {
                fr.end - chunk_start
            } else {
                CHUNK_SIZE as u64
            };
            f(Block::new(m_addr as *const u8, CHUNK_SIZE as i32, true)
                .take_first64(end_off)
                .drop_first64(start_off));
            chunk_start += CHUNK_SIZE;
            if chunk_start >= fr.end || chunk_start == 0 {
                break;
            }
        }
        Ok(())
    }
}
