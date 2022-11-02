mod context;

pub use context::Context;

use std::fs::File as StdFile;
use std::io;
use std::os::unix::io::AsRawFd;
use std::rc::Rc;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use mem::{
    block::Block, block_seq::BlockSeq, io::read_full_to_blocks, AccessType, Addr, HUGE_PAGE_SIZE,
    PAGE_SIZE,
};
use memmap::file::MemmapFile;
use segment::{Gap, Set, SetOperations, CHUNK_MASK, CHUNK_SHIFT, CHUNK_SIZE};
use usage::MemoryKind;
use utils::{bail_libc, FileRange, Range, SysError, SysResult};

#[derive(Copy, Clone, Debug)]
pub enum Direction {
    BottomUp,
    TopDown,
}

#[derive(PartialEq, Copy, Clone, Debug)]
struct UsageInfo {
    kind: MemoryKind,
    known_committed: bool,
}

type UsageSet = Set<u64, UsageInfo>;

struct UsageInfoSetOperations;
impl SetOperations for UsageInfoSetOperations {
    type K = u64;
    type V = UsageInfo;
    fn merge(
        &self,
        _: Range<Self::K>,
        v1: &Self::V,
        _: Range<Self::K>,
        v2: &Self::V,
    ) -> Option<Self::V> {
        if v1 == v2 {
            Some(*v1)
        } else {
            None
        }
    }

    fn split(&self, _: Range<Self::K>, v: &Self::V, _: Self::K) -> (Self::V, Self::V) {
        (*v, *v)
    }
}

#[derive(Default, Debug)]
pub struct MemoryFileOpts {
    delayed_eviction: DelayedEviction,
    use_host_memcg_pressure: bool,
    manual_zeroing: bool,
}

#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq)]
enum DelayedEviction {
    Defaulted,
    Disabled,
    Enabled,
    Manual,
}

impl Default for DelayedEviction {
    fn default() -> DelayedEviction {
        DelayedEviction::Defaulted
    }
}

#[derive(Debug)]
pub struct AllocOpts {
    pub kind: MemoryKind,
    pub dir: Direction,
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct ReclaimSetValue;

struct ReclaimSetOperations;
impl SetOperations for ReclaimSetOperations {
    type K = u64;
    type V = ReclaimSetValue;
    fn merge(
        &self,
        _: Range<Self::K>,
        _: &Self::V,
        _: Range<Self::K>,
        _: &Self::V,
    ) -> Option<Self::V> {
        Some(ReclaimSetValue {})
    }

    fn split(&self, _: Range<Self::K>, _: &Self::V, _: Self::K) -> (Self::V, Self::V) {
        (ReclaimSetValue {}, ReclaimSetValue {})
    }
}

pub trait MemoryFileProvider {
    fn memory_file(&self) -> &Rc<RwLock<MemoryFile>>;
    fn memory_file_read_lock(&self) -> RwLockReadGuard<'_, MemoryFile>;
    fn memory_file_write_lock(&self) -> RwLockWriteGuard<'_, MemoryFile>;
}

#[derive(Debug)]
pub struct MemoryFile {
    file: Box<StdFile>,
    file_size: i64,
    mappings: Vec<u64>,
    usage: UsageSet,
    opts: MemoryFileOpts,
}

impl MemoryFile {
    pub fn new(file: StdFile, mut opts: MemoryFileOpts) -> io::Result<Self> {
        match opts.delayed_eviction {
            DelayedEviction::Defaulted => opts.delayed_eviction = DelayedEviction::Enabled,
            DelayedEviction::Disabled | DelayedEviction::Manual => {
                opts.use_host_memcg_pressure = false
            }
            DelayedEviction::Enabled => (),
        };
        let usage_ops = UsageInfoSetOperations;
        file.set_len(0)?;
        let mf = MemoryFile {
            file: Box::new(file),
            file_size: 0,
            mappings: Vec::new(),
            usage: UsageSet::new(Box::new(usage_ops)),
            opts,
        };
        let m = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                PAGE_SIZE as usize,
                libc::PROT_EXEC,
                libc::MAP_SHARED,
                mf.file.as_raw_fd(),
                0,
            )
        };
        if m == libc::MAP_FAILED {
            logger::warn!("Failed to pre-map MemoryFile PROT_EXEC");
        } else if unsafe { libc::munmap(m, PAGE_SIZE as usize) } < 0 {
            panic!("failed to unmap PROT_EXEC MemoryFile mapping");
        }
        Ok(mf)
    }

    pub fn allocate(&mut self, length: u64, opts: AllocOpts) -> SysResult<FileRange> {
        if length == 0 || length % PAGE_SIZE as u64 != 0 {
            panic!("invalid allocation length: {}", length);
        }

        let alignment = if length >= HUGE_PAGE_SIZE {
            HUGE_PAGE_SIZE as u64
        } else {
            PAGE_SIZE as u64
        };

        let fr = self
            .find_available_range(length, alignment, opts.dir)
            .ok_or_else(|| SysError::new(libc::ENOMEM))?;

        if fr.end as i64 > self.file_size {
            let new_file_size = (fr.end as i64 + CHUNK_MASK) & !CHUNK_MASK;
            self.file
                .set_len(new_file_size as u64)
                .map_err(|e| SysError::new(e.raw_os_error().unwrap()))?;
            self.file_size = new_file_size as i64;
            let mut new_mappings = self.mappings.clone();
            new_mappings.resize(new_file_size as usize >> CHUNK_SHIFT, 0);
            self.mappings = new_mappings;
        }

        if self.opts.manual_zeroing {
            todo!();
        }

        if !self.usage.add(
            fr,
            UsageInfo {
                kind: opts.kind,
                known_committed: false,
            },
        ) {
            panic!(
                "allocating {:?}: failed to insert into usage set: {:?}",
                fr, &self.usage
            );
        }
        Ok(fr)
    }

    pub fn allocate_and_fill(
        &mut self,
        length: u64,
        kind: MemoryKind,
        r: impl mem::io::Reader,
    ) -> SysResult<FileRange> {
        let mut fr = self.allocate(
            length,
            AllocOpts {
                kind,
                dir: Direction::BottomUp,
            },
        )?;
        let dsts = self.map_internal(fr, AccessType::write())?;
        let n = read_full_to_blocks(r, dsts)?;
        let un = Addr(n as u64).round_down().0 as u64;
        if un < length {
            fr.end = fr.start + un;
        }
        Ok(fr)
    }

    fn get_chunk_mapping(&mut self, chunk: i32) -> Result<u64, SysError> {
        // NOTE: maybe unnessary. Just in case another thread have already mapped the chunk.
        let m = self.mappings[chunk as usize];
        if m != 0 {
            return Ok(m);
        }
        let m = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                CHUNK_SIZE as usize,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                self.file.as_raw_fd(),
                (chunk << CHUNK_SHIFT) as i64,
            )
        };
        if m == libc::MAP_FAILED {
            bail_libc!(libc::EINVAL);
        }
        self.mappings[chunk as usize] = m as u64;
        Ok(m as u64)
    }

    fn for_each_mapping_slice<F: FnMut(&mut [u8])>(
        &mut self,
        file_range: &FileRange,
        mut f: F,
    ) -> SysResult<()> {
        let start = file_range.start & !CHUNK_MASK as u64;
        for chunk_start in (start..file_range.end).step_by(CHUNK_SIZE as usize) {
            let chunk = chunk_start >> CHUNK_SHIFT;
            let mut m = self.mappings[chunk as usize];
            if m == 0 {
                m = self.get_chunk_mapping(chunk as i32)?;
            }
            let start_off = if chunk_start < file_range.start {
                file_range.start - chunk_start
            } else {
                0
            } as usize;
            let end_off = if chunk_start + CHUNK_SIZE as u64 > file_range.end {
                file_range.end - chunk_start
            } else {
                CHUNK_SIZE as u64
            } as usize;
            let bs =
                &mut unsafe { std::slice::from_raw_parts_mut(m as *mut u8, CHUNK_SIZE as usize) }
                    [start_off..end_off];
            f(bs);
        }
        Ok(())
    }

    fn find_available_range(
        &self,
        length: u64,
        alignment: u64,
        dir: Direction,
    ) -> Option<FileRange> {
        match dir {
            Direction::BottomUp => find_available_range_bottom_up(&self.usage, length, alignment),
            Direction::TopDown => {
                find_available_range_top_down(&self.usage, self.file_size, length, alignment)
            }
        }
    }

    pub fn should_cache_evictable(&self) -> bool {
        self.opts.delayed_eviction == DelayedEviction::Manual || self.opts.use_host_memcg_pressure
    }

    pub fn total_usage(&self) -> nix::Result<u64> {
        let stat = nix::sys::stat::fstat(self.file.as_raw_fd())?;
        Ok((stat.st_blocks as u64) * 512)
    }

    pub fn total_size(&self) -> u64 {
        self.file_size as u64
    }
}

impl MemmapFile for MemoryFile {
    fn map_internal(&mut self, fr: FileRange, at: AccessType) -> SysResult<BlockSeq> {
        if !fr.is_well_formed() || fr.is_empty() {
            panic!("invalid range: {:?}", fr);
        }
        if at.execute {
            bail_libc!(libc::EACCES);
        }
        let chunks = ((fr.end + CHUNK_MASK as u64) >> CHUNK_SHIFT) - (fr.start >> CHUNK_SHIFT);
        if chunks == 1 {
            let mut seq = BlockSeq::default();
            self.for_each_mapping_slice(&fr, |bs| {
                seq = BlockSeq::from_block(Block::from_slice(bs, false))
            })?;
            Ok(seq)
        } else {
            let mut blocks = Vec::new();
            self.for_each_mapping_slice(&fr, |bs| blocks.push(Block::from_slice(bs, false)))?;
            Ok(BlockSeq::from_blocks(blocks))
        }
    }

    fn fd(&self) -> (i32, bool) {
        (self.file.as_raw_fd(), false)
    }

    fn close(&self) {
        panic!("MemoryFile should not be closed");
    }
}

fn find_available_range_bottom_up(
    usage: &UsageSet,
    length: u64,
    alignment: u64,
) -> Option<FileRange> {
    let alignment_mask = alignment - 1;
    let mut gap_maybe = usage.first_gap().or_else(|| Some(Gap::minimum()));
    while let Some(gap) = gap_maybe {
        let start = (gap.start() + alignment_mask) & !alignment_mask;
        let end = start.checked_add(length)?;
        if end as i64 <= 0 {
            return None;
        }
        if end <= gap.end() {
            return Some(FileRange { start, end });
        }
        gap_maybe = usage.next_large_enough_gap(&gap, length);
    }
    panic!(
        "next_large_enough_gap didn't return a gap at the end, length: {}",
        length
    );
}

fn find_available_range_top_down(
    usage: &UsageSet,
    mut file_size: i64,
    length: u64,
    alignment: u64,
) -> Option<FileRange> {
    let alignment_mask = alignment - 1;
    let last_gap = usage.last_gap().unwrap();
    let mut gap = last_gap;
    loop {
        let end = std::cmp::min(gap.end(), file_size as u64);
        let unaligned_start = match end.checked_sub(length) {
            Some(v) => v,
            None => break,
        };
        let start = unaligned_start & !alignment_mask;
        if start >= gap.start() {
            return Some(FileRange {
                start,
                end: start + length,
            });
        }
        match usage.prev_large_enough_gap(&gap, length) {
            Some(g) => gap = g,
            None => break,
        }
    }

    let min = last_gap.start();
    let min = (min + alignment_mask) & !alignment_mask;
    min.checked_add(length)?;

    loop {
        let new_file_size = if file_size == 0 {
            CHUNK_SIZE
        } else {
            file_size.checked_mul(2)?
        };
        file_size = new_file_size;
        if (file_size as u64) < length {
            continue;
        }
        let unaligned_start = file_size as u64 - length;
        let start = unaligned_start & !alignment_mask;
        if start >= min {
            return Some(FileRange {
                start,
                end: start + length,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use segment::SegmentDataSlices;

    use super::*;

    const PAGE: u64 = PAGE_SIZE as u64;
    const HUGE_PAGE: u64 = HUGE_PAGE_SIZE as u64;
    const TOP_PAGE: u64 = (1 << 63) - PAGE;

    type UsageSegmentDataSlices = SegmentDataSlices<u64, UsageInfo>;

    struct Test {
        usage: UsageSegmentDataSlices,
        file_size: u64,
        length: u64,
        alignment: u64,
        direction: Direction,
        want: Result<u64, ()>,
    }

    #[test]
    fn find_unallocated_range() {
        for test in &[
            Test {
                usage: UsageSegmentDataSlices::default(),
                file_size: 0,
                length: PAGE,
                alignment: PAGE,
                direction: Direction::BottomUp,
                want: Ok(0),
            },
            Test {
                usage: UsageSegmentDataSlices::default(),
                file_size: 0,
                length: PAGE,
                alignment: PAGE,
                direction: Direction::TopDown,
                want: Ok(CHUNK_SIZE as u64 - PAGE),
            },
            Test {
                usage: UsageSegmentDataSlices {
                    start: vec![PAGE],
                    end: vec![2 * PAGE],
                    values: vec![UsageInfo {
                        kind: MemoryKind::System,
                        known_committed: false,
                    }],
                },
                file_size: 0,
                length: PAGE,
                alignment: PAGE,
                direction: Direction::BottomUp,
                want: Ok(0),
            },
            Test {
                usage: UsageSegmentDataSlices {
                    start: vec![PAGE],
                    end: vec![2 * PAGE],
                    values: vec![UsageInfo {
                        kind: MemoryKind::System,
                        known_committed: false,
                    }],
                },
                file_size: 2 * PAGE,
                length: PAGE,
                alignment: PAGE,
                direction: Direction::TopDown,
                want: Ok(0),
            },
            Test {
                usage: UsageSegmentDataSlices {
                    start: vec![0],
                    end: vec![PAGE],
                    values: vec![UsageInfo {
                        kind: MemoryKind::System,
                        known_committed: false,
                    }],
                },
                file_size: 2 * PAGE,
                length: PAGE,
                alignment: PAGE,
                direction: Direction::TopDown,
                want: Ok(PAGE),
            },
            Test {
                usage: UsageSegmentDataSlices {
                    start: vec![0, PAGE],
                    end: vec![PAGE, 2 * PAGE],
                    values: vec![
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                    ],
                },
                file_size: 0,
                length: PAGE,
                alignment: PAGE,
                direction: Direction::BottomUp,
                want: Ok(2 * PAGE),
            },
            Test {
                usage: UsageSegmentDataSlices {
                    start: vec![0, PAGE],
                    end: vec![PAGE, 2 * PAGE],
                    values: vec![
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                    ],
                },
                file_size: 2 * PAGE,
                length: PAGE,
                alignment: PAGE,
                direction: Direction::TopDown,
                want: Ok(3 * PAGE),
            },
            Test {
                usage: UsageSegmentDataSlices {
                    start: vec![0, PAGE, 2 * PAGE],
                    end: vec![PAGE, 2 * PAGE, 3 * PAGE],
                    values: vec![
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                    ],
                },
                file_size: 0,
                length: PAGE,
                alignment: PAGE,
                direction: Direction::BottomUp,
                want: Ok(3 * PAGE),
            },
            Test {
                usage: UsageSegmentDataSlices {
                    start: vec![0, PAGE, 2 * PAGE],
                    end: vec![PAGE, 2 * PAGE, 3 * PAGE],
                    values: vec![
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                    ],
                },
                file_size: 3 * PAGE,
                length: PAGE,
                alignment: PAGE,
                direction: Direction::TopDown,
                want: Ok(5 * PAGE),
            },
            Test {
                usage: UsageSegmentDataSlices {
                    start: vec![0, 2 * PAGE],
                    end: vec![PAGE, 3 * PAGE],
                    values: vec![
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                    ],
                },
                file_size: 3 * PAGE,
                length: PAGE,
                alignment: PAGE,
                direction: Direction::TopDown,
                want: Ok(PAGE),
            },
            Test {
                usage: UsageSegmentDataSlices {
                    start: vec![0, 2 * PAGE],
                    end: vec![PAGE, 3 * PAGE],
                    values: vec![
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                    ],
                },
                file_size: 0,
                length: 2 * PAGE,
                alignment: PAGE,
                direction: Direction::BottomUp,
                want: Ok(3 * PAGE),
            },
            Test {
                usage: UsageSegmentDataSlices {
                    start: vec![0, 2 * PAGE],
                    end: vec![PAGE, 3 * PAGE],
                    values: vec![
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                    ],
                },
                file_size: 3 * PAGE,
                length: 2 * PAGE,
                alignment: PAGE,
                direction: Direction::TopDown,
                want: Ok(4 * PAGE),
            },
            Test {
                usage: UsageSegmentDataSlices {
                    start: vec![0, HUGE_PAGE + PAGE],
                    end: vec![PAGE, HUGE_PAGE + 2 * PAGE],
                    values: vec![
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                    ],
                },
                file_size: 0,
                length: HUGE_PAGE,
                alignment: HUGE_PAGE,
                direction: Direction::BottomUp,
                want: Ok(2 * HUGE_PAGE),
            },
            Test {
                usage: UsageSegmentDataSlices {
                    start: vec![0, HUGE_PAGE + PAGE],
                    end: vec![PAGE, HUGE_PAGE + 2 * PAGE],
                    values: vec![
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                    ],
                },
                file_size: HUGE_PAGE + 2 * PAGE,
                length: HUGE_PAGE,
                alignment: HUGE_PAGE,
                direction: Direction::TopDown,
                want: Ok(3 * HUGE_PAGE),
            },
            Test {
                usage: UsageSegmentDataSlices {
                    start: vec![0, 2 * HUGE_PAGE + PAGE],
                    end: vec![PAGE, 2 * HUGE_PAGE + 2 * PAGE],
                    values: vec![
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                    ],
                },
                file_size: 2 * HUGE_PAGE + 2 * PAGE,
                length: HUGE_PAGE,
                alignment: HUGE_PAGE,
                direction: Direction::TopDown,
                want: Ok(HUGE_PAGE),
            },
            Test {
                usage: UsageSegmentDataSlices::default(),
                file_size: PAGE,
                length: 4 * PAGE,
                alignment: PAGE,
                direction: Direction::BottomUp,
                want: Ok(0),
            },
            Test {
                usage: UsageSegmentDataSlices::default(),
                file_size: PAGE,
                length: 4 * PAGE,
                alignment: PAGE,
                direction: Direction::TopDown,
                want: Ok(0),
            },
            Test {
                usage: UsageSegmentDataSlices {
                    start: vec![PAGE, 3 * PAGE],
                    end: vec![2 * PAGE, 4 * PAGE],
                    values: vec![
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                    ],
                },
                file_size: 4 * PAGE,
                length: PAGE,
                alignment: PAGE,
                direction: Direction::TopDown,
                want: Ok(2 * PAGE),
            },
            Test {
                usage: UsageSegmentDataSlices {
                    start: vec![PAGE, 4 * PAGE, 7 * PAGE],
                    end: vec![2 * PAGE, 5 * PAGE, 8 * PAGE],
                    values: vec![
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                    ],
                },
                file_size: 8 * PAGE,
                length: PAGE,
                alignment: PAGE,
                direction: Direction::TopDown,
                want: Ok(6 * PAGE),
            },
            Test {
                usage: UsageSegmentDataSlices {
                    start: vec![PAGE, 3 * PAGE, 5 * PAGE],
                    end: vec![2 * PAGE, 4 * PAGE, 6 * PAGE],
                    values: vec![
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                    ],
                },
                file_size: 6 * PAGE,
                length: PAGE,
                alignment: PAGE,
                direction: Direction::TopDown,
                want: Ok(4 * PAGE),
            },
            Test {
                usage: UsageSegmentDataSlices {
                    start: vec![PAGE, 3 * PAGE],
                    end: vec![2 * PAGE, 4 * PAGE],
                    values: vec![
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                    ],
                },
                file_size: 8 * PAGE,
                length: PAGE,
                alignment: PAGE,
                direction: Direction::TopDown,
                want: Ok(7 * PAGE),
            },
            Test {
                usage: UsageSegmentDataSlices {
                    start: vec![PAGE, TOP_PAGE - PAGE],
                    end: vec![2 * PAGE, TOP_PAGE],
                    values: vec![
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                    ],
                },
                file_size: TOP_PAGE,
                length: PAGE,
                alignment: PAGE,
                direction: Direction::TopDown,
                want: Ok(TOP_PAGE - 2 * PAGE),
            },
            Test {
                usage: UsageSegmentDataSlices {
                    start: vec![0, 3 * PAGE],
                    end: vec![2 * PAGE, 4 * PAGE],
                    values: vec![
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                        UsageInfo {
                            kind: MemoryKind::System,
                            known_committed: false,
                        },
                    ],
                },
                file_size: 0,
                length: PAGE,
                alignment: PAGE,
                direction: Direction::BottomUp,
                want: Ok(2 * PAGE),
            },
            Test {
                usage: UsageSegmentDataSlices {
                    start: vec![PAGE],
                    end: vec![TOP_PAGE],
                    values: vec![UsageInfo {
                        kind: MemoryKind::System,
                        known_committed: false,
                    }],
                },
                file_size: TOP_PAGE,
                length: 2 * PAGE,
                alignment: PAGE,
                direction: Direction::BottomUp,
                want: Err(()),
            },
            Test {
                usage: UsageSegmentDataSlices {
                    start: vec![PAGE],
                    end: vec![TOP_PAGE],
                    values: vec![UsageInfo {
                        kind: MemoryKind::System,
                        known_committed: false,
                    }],
                },
                file_size: TOP_PAGE,
                length: 2 * PAGE,
                alignment: PAGE,
                direction: Direction::TopDown,
                want: Err(()),
            },
        ] {
            let dummy_file = StdFile::open("/tmp").unwrap();
            let usage_info_ops = UsageInfoSetOperations;
            let mut mf = MemoryFile {
                file: Box::new(dummy_file),
                file_size: test.file_size as i64,
                mappings: Vec::new(),
                usage: UsageSet::new(Box::new(usage_info_ops)),
                opts: MemoryFileOpts::default(),
            };

            let res = mf.usage.import_sorted_slices(&test.usage);
            assert!(res.is_ok());

            match mf.find_available_range(test.length, test.alignment, test.direction) {
                Some(fr) => {
                    assert!(test.want.is_ok());
                    assert_eq!(fr.start, test.want.unwrap());
                    assert_eq!(fr.end, test.want.unwrap() + test.length);
                }
                None => {
                    assert!(test.want.is_err());
                }
            }
        }
    }
}
