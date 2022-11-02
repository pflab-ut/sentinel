use std::{
    rc::Rc,
    sync::{RwLock, RwLockReadGuard, RwLockWriteGuard},
};

use mem::{
    block_seq::{copy_seq, BlockSeq, BlockSeqView},
    io, AccessType, Addr, AddrRange, IoSequence,
};
use memmap::{
    file::MemmapFile,
    mapping_set::{MappingSet, MappingSetOperations, SetU64MappingOfRange},
    InvalidateOpts, Mappable, MappableRange, Translation,
};
use segment::SegOrGap;
use time::Time;
use utils::{bail_libc, SysError, SysResult};

use crate::{
    attr::{AttrMask, FilePermissions, StableAttr, UnstableAttr},
    context::Context,
    host::RegularFileObject,
    inode,
    inode_operations::RenameUnderParents,
    mount::MountSource,
    offset::{read_end_offset, write_end_offset},
    DirentRef, File, FileFlags, InodeOperations,
};

use super::{FileRangeSet, FileRangeSetOperations, HostFileMapper, SetU64Operations};

#[derive(Debug)]
pub struct CachingInodeMappable {
    pub backing_file: Rc<RegularFileObject>,
    pub host_file_mapper: HostFileMapper,
}

impl MemmapFile for CachingInodeMappable {
    #[inline]
    fn map_internal(&mut self, fr: utils::FileRange, at: AccessType) -> SysResult<BlockSeq> {
        let (fd, should_close) = self.backing_file.fd();
        let res = self.host_file_mapper.map_internal(fr, fd, at.write);
        if should_close {
            self.backing_file.close();
        }
        res
    }
    #[inline]
    fn fd(&self) -> (i32, bool) {
        self.backing_file.fd()
    }
    #[inline]
    fn close(&self) {
        self.backing_file.close()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CachingInodeOperationsOptions {
    pub force_page_cache: bool,
    pub limit_host_fd_translation: bool,
}

#[derive(Debug)]
pub struct CachingInodeOperations {
    opts: CachingInodeOperationsOptions,
    uattr: UnstableAttr,
    mappings: MappingSet,
    cache: FileRangeSet,
    mappable: Rc<RwLock<CachingInodeMappable>>,
}

impl InodeOperations for CachingInodeOperations {
    fn lookup(&mut self, _: &str, _: &dyn Context) -> SysResult<DirentRef> {
        unimplemented!()
    }
    fn get_file(&self, _: DirentRef, _: FileFlags) -> SysResult<File> {
        unimplemented!()
    }
    fn unstable_attr(&self, _: &Rc<MountSource>, _: StableAttr) -> SysResult<UnstableAttr> {
        Ok(self.uattr)
    }
    fn get_link(&self) -> SysResult<DirentRef> {
        unimplemented!()
    }
    fn read_link(&self) -> SysResult<String> {
        unimplemented!()
    }
    fn truncate(&mut self, size: i64, ctx: &dyn Context) -> SysResult<()> {
        let now = ctx.now();
        let mut masked = AttrMask {
            size: true,
            ..AttrMask::default()
        };
        let mut attr = UnstableAttr {
            size,
            ..UnstableAttr::default()
        };
        if self.uattr.perms.has_set_uid_or_gid() {
            masked.perms = true;
            attr.perms = self.uattr.perms;
            attr.perms.drop_set_uid_and_maybe_gid();
            self.uattr.perms = attr.perms;
        }
        self.mappable
            .write()
            .unwrap()
            .backing_file
            .set_masked_attributes(masked, attr)?;
        let old_size = self.uattr.size;
        self.uattr.size = size;
        self.touch_modification_and_status_change_time(now);

        if size >= old_size {
            return Ok(());
        }

        let new_page_end = Addr(size as u64).round_up().unwrap();
        let old_page_end = Addr(old_size as u64).round_up().unwrap();

        if new_page_end != old_page_end {
            self.mappings.invalidate(
                MappableRange {
                    start: new_page_end.0,
                    end: old_page_end.0,
                },
                InvalidateOpts {
                    invalidate_private: true,
                },
                &ctx.mm(),
            );
        }
        self.cache.truncate(size as u64, ctx);
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
    ) -> SysResult<File> {
        unreachable!()
    }
    fn rename(
        &self,
        _: RenameUnderParents<&mut inode::Inode>,
        _: &str,
        _: String,
        _: bool,
        _: &dyn Context,
    ) -> SysResult<()> {
        logger::warn!("renaming is only allowed for the files that were created by user");
        bail_libc!(libc::EPERM)
    }
    fn add_link(&self) {}
    fn drop_link(&self) {}
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

fn max_fill_range(required: MappableRange, mut optional: MappableRange) -> MappableRange {
    const MAX_READAHEAD: u64 = 64 << 10;
    if required.len() >= MAX_READAHEAD {
        required
    } else if optional.len() <= MAX_READAHEAD {
        optional
    } else {
        optional.start = required.start;
        if optional.len() <= MAX_READAHEAD {
            optional
        } else {
            optional.end = optional.start + MAX_READAHEAD;
            optional
        }
    }
}

impl Mappable for CachingInodeOperations {
    fn translate(
        &self,
        required: MappableRange,
        optional: MappableRange,
        _: AccessType,
    ) -> (Vec<Translation>, SysResult<()>) {
        if !self.use_host_page_cache() {
            todo!();
        }
        let mr = if self.opts.limit_host_fd_translation {
            max_fill_range(required, optional)
        } else {
            optional
        };
        (
            vec![Translation::new(
                mr,
                Rc::<RwLock<CachingInodeMappable>>::downgrade(&self.mappable),
                mr.start,
                AccessType::any_access(),
            )],
            Ok(()),
        )
    }

    fn add_mapping(&mut self, ar: AddrRange, offset: u64, writable: bool) -> SysResult<()> {
        let _mapped = self.mappings.add_mapping(ar, offset, writable);
        if !self.use_host_page_cache() {
            todo!();
        }
        Ok(())
    }

    fn remove_mapping(&mut self, ar: AddrRange, offset: u64, writable: bool) {
        let _unmapped = self.mappings.remove_mapping(ar, offset, writable);
        if self.use_host_page_cache() {
            return;
        }
        todo!();
    }

    fn copy_mapping(
        &mut self,
        _: AddrRange,
        dst_ar: AddrRange,
        offset: u64,
        writable: bool,
    ) -> SysResult<()> {
        self.add_mapping(dst_ar, offset, writable)
    }
}

impl CachingInodeOperations {
    pub fn new(
        backing_file: Rc<RegularFileObject>,
        uattr: UnstableAttr,
        opts: CachingInodeOperationsOptions,
    ) -> Self {
        let mops = MappingSetOperations;
        let cops = FileRangeSetOperations;
        let mappable = Rc::new(RwLock::new(CachingInodeMappable {
            backing_file,
            host_file_mapper: HostFileMapper::default(),
        }));
        Self {
            opts,
            uattr,
            mappings: MappingSet::new(Box::new(mops)),
            cache: FileRangeSet::new(Box::new(cops)),
            mappable,
        }
    }

    fn use_host_page_cache(&self) -> bool {
        !self.opts.force_page_cache
    }

    fn touch_modification_and_status_change_time(&mut self, now: Time) {
        self.uattr.modification_time = now;
        self.uattr.status_change_time = now;
    }

    fn touch_access_time(&mut self, inode: &inode::Inode, ctx: &dyn Context) {
        if inode.mount_source().flags().no_atime {
            return;
        }
        self.uattr.access_time = ctx.now();
    }

    pub fn caching_inode_mappable(&self) -> RwLockReadGuard<'_, CachingInodeMappable> {
        self.mappable.read().unwrap()
    }

    pub fn caching_inode_mappable_mut(&self) -> RwLockWriteGuard<'_, CachingInodeMappable> {
        self.mappable.write().unwrap()
    }

    pub fn read(
        &mut self,
        file: &File,
        dst: &IoSequence,
        offset: i64,
        ctx: &dyn Context,
    ) -> SysResult<usize> {
        if dst.num_bytes() == 0 {
            return Ok(0);
        }
        let size = self.uattr.size;
        if offset >= size {
            return Ok(0);
        }
        let mut reader = InodeReadWriter {
            c: self,
            offset,
            ctx,
        };
        let res = dst.copy_out_from(&mut reader);
        let dirent = file.dirent();
        let dirent = dirent.borrow();
        self.touch_access_time(dirent.inode(), ctx);
        res
    }

    pub fn write(&mut self, src: &IoSequence, offset: i64, ctx: &dyn Context) -> SysResult<usize> {
        if src.num_bytes() == 0 {
            return Ok(0);
        }
        self.touch_modification_and_status_change_time(ctx.now());
        let mut writer = InodeReadWriter {
            c: self,
            offset,
            ctx,
        };
        src.copy_in_to(&mut writer)
    }
}

struct InodeReadWriter<'a> {
    c: &'a CachingInodeOperations,
    offset: i64,
    ctx: &'a dyn Context,
}

impl io::Reader for InodeReadWriter<'_> {
    fn read_to_blocks(&mut self, mut dsts: BlockSeqView) -> SysResult<usize> {
        if self.offset >= self.c.uattr.size {
            return Ok(0);
        }
        let end = read_end_offset(self.offset, dsts.num_bytes() as i64, self.c.uattr.size);
        if end == self.offset {
            return Ok(0);
        }
        let should_cache_evictable = self
            .ctx
            .memory_file_provider()
            .memory_file_read_lock()
            .should_cache_evictable();
        let fill_cache = !self.c.use_host_page_cache() && should_cache_evictable;
        let mut seg = self.c.cache.find_segment(self.offset as u64);
        let mut gap = self.c.cache.find_gap(self.offset as u64);
        let mut done = 0;
        while self.offset < end {
            let mr = MappableRange {
                start: self.offset as u64,
                end: end as u64,
            };
            if let Some(seg_inner) = seg {
                let ims = {
                    let mut mf = self.ctx.memory_file_provider().memory_file_write_lock();
                    mf.map_internal(
                        self.c
                            .cache
                            .file_range_of(&seg_inner, seg_inner.range().intersect(&mr)),
                        AccessType::read(),
                    )?
                };
                let n = copy_seq(dsts, ims.as_view())?;
                done += n;
                self.offset += n as i64;
                dsts.drop_first(n as u64);
                match self.c.cache.next_non_empty(&seg_inner) {
                    Some(SegOrGap::Segment(s)) => {
                        seg = Some(s);
                        gap = None;
                    }
                    Some(SegOrGap::Gap(g)) => {
                        seg = None;
                        gap = Some(g);
                    }
                    None => {
                        seg = None;
                        gap = None;
                    }
                }
            } else if let Some(gap_inner) = gap {
                let gap_mr = gap_inner.range().intersect(&mr);
                if fill_cache {
                    todo!()
                } else {
                    let dst = dsts.take_first(gap_mr.len());
                    let n = self
                        .c
                        .mappable
                        .write()
                        .unwrap()
                        .backing_file
                        .read_to_blocks_at(dst, gap_mr.start)?;
                    done += n;
                    self.offset += n as i64;
                    dsts.drop_first(n as u64);
                    if n != dst.num_bytes() as usize {
                        return Ok(n);
                    }
                    seg = self.c.cache.next_segment_of_gap(&gap_inner);
                    gap = None;
                }
            } else {
                unreachable!("infinity loop");
            }
        }
        Ok(done)
    }
}

impl io::Writer for InodeReadWriter<'_> {
    fn write_from_blocks(&mut self, mut srcs: BlockSeqView) -> SysResult<usize> {
        let end = write_end_offset(self.offset, srcs.num_bytes() as i64);
        if end == self.offset {
            return Ok(0);
        }
        let mut done = 0;
        let mut seg = self.c.cache.find_segment(self.offset as u64);
        let mut gap = self.c.cache.find_gap(self.offset as u64);
        while self.offset < end {
            let mr = MappableRange {
                start: self.offset as u64,
                end: end as u64,
            };
            if seg.map_or(false, |s| s.start() < mr.end) {
                let seg_inner = seg.unwrap();
                let seg_mr = seg_inner.range().intersect(&mr);
                let ims = {
                    let mut mf = self.ctx.memory_file_provider().memory_file_write_lock();
                    mf.map_internal(
                        self.c.cache.file_range_of(&seg_inner, seg_mr),
                        AccessType::write(),
                    )?
                };
                let n = copy_seq(ims.as_view(), srcs)?;
                done += n;
                self.offset += n as i64;
                srcs.drop_first(n as u64);
                match self.c.cache.next_non_empty(&seg_inner) {
                    Some(SegOrGap::Segment(s)) => {
                        seg = Some(s);
                        gap = None;
                    }
                    Some(SegOrGap::Gap(g)) => {
                        seg = None;
                        gap = Some(g);
                    }
                    None => {
                        seg = None;
                        gap = None;
                    }
                }
            } else if gap.map_or(false, |g| g.start() < mr.end) {
                let gap_inner = gap.unwrap();
                let gap_mr = gap_inner.range().intersect(&mr);
                let src = srcs.take_first(gap_mr.len());
                let n = self
                    .c
                    .mappable
                    .write()
                    .unwrap()
                    .backing_file
                    .write_from_blocks_at(src, gap_mr.start)?;
                done += n;
                self.offset += n as i64;
                srcs.drop_first(n as u64);
                if n != src.num_bytes() as usize {
                    return Ok(n);
                }
                seg = self.c.cache.next_segment_of_gap(&gap_inner);
                gap = None;
            } else {
                unreachable!("infinity loop");
            }
        }
        Ok(done)
    }
}
