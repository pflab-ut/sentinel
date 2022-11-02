use std::{
    cmp::{max, min},
    rc::Rc,
    sync::RwLock,
};

use mem::{
    block_seq::{copy_seq, zero_seq, BlockSeqView},
    io, AccessType, Addr, AddrRange, IoSequence,
};
use memmap::{
    file::MemmapFile,
    mapping_set::{MappingSet, MappingSetOperations, SetU64MappingOfRange},
    mmap_opts::MmapOpts,
    InvalidateOpts, Mappable, MappableRange, Translation,
};
use pgalloc::{AllocOpts, Direction};
use segment::SegOrGap;
use usage::MemoryKind;
use utils::{bail_libc, err_libc, SysError, SysResult};

use crate::{
    attr::{FilePermissions, StableAttr, UnstableAttr},
    context::Context,
    dentry::DentrySerializer,
    fsutils::{seek_with_dir_cursor, FileRangeSet, FileRangeSetOperations, SetU64Operations},
    inode::Inode,
    inode_operations::RenameUnderParents,
    mount::{MountSource, MountSourceFlags},
    offset::{offset_page_end, read_end_offset, write_end_offset},
    seek::SeekWhence,
    DirentRef, File, FileFlags, FileOperations, InodeOperations, ReaddirError, ReaddirResult,
};

// RegularFile implements InodeOperations for a regular tmpfs file.
#[derive(Debug)]
pub struct RegularFile {
    attr: RwLock<UnstableAttr>,
    mem_usage: MemoryKind,
    data: FileRangeSet,
    mappings: MappingSet,
    seals: i32,
}

impl Mappable for RegularFile {
    fn translate(
        &self,
        _required: MappableRange,
        _optional: MappableRange,
        _at: AccessType,
    ) -> (Vec<Translation>, SysResult<()>) {
        todo!()
    }
    fn add_mapping(&mut self, _ar: AddrRange, _offset: u64, _writable: bool) -> SysResult<()> {
        todo!()
    }
    fn remove_mapping(&mut self, _ar: AddrRange, _offset: u64, _writable: bool) {
        todo!()
    }
    fn copy_mapping(
        &mut self,
        _src_ar: AddrRange,
        _dst_ar: AddrRange,
        _offset: u64,
        _writable: bool,
    ) -> SysResult<()> {
        todo!()
    }
}

impl InodeOperations for RegularFile {
    fn lookup(&mut self, _: &str, _: &dyn Context) -> SysResult<DirentRef> {
        bail_libc!(libc::ENOTDIR);
    }

    fn get_file(&self, dirent: DirentRef, mut flags: FileFlags) -> SysResult<File> {
        let stable_attr = {
            let dirent = dirent.borrow();
            dirent.inode().stable_attr()
        };
        if stable_attr.is_socket() {
            bail_libc!(libc::ENXIO);
        }
        flags.pread = true;
        flags.pwrite = true;
        Ok(File::new(flags, Box::new(RegularFileOperations { dirent })))
    }

    fn unstable_attr(&self, _: &Rc<MountSource>, _: StableAttr) -> SysResult<UnstableAttr> {
        Ok(UnstableAttr {
            usage: self.data.span() as i64,
            ..*self.attr.read().unwrap()
        })
    }

    fn get_link(&self) -> SysResult<DirentRef> {
        err_libc!(libc::ENOLINK)
    }

    fn read_link(&self) -> SysResult<String> {
        err_libc!(libc::ENOLINK)
    }

    fn truncate(&mut self, size: i64, ctx: &dyn Context) -> SysResult<()> {
        let mut attr = self.attr.write().unwrap();
        let old_size = attr.size;

        if (size > old_size && self.seals & linux::F_SEAL_GROW != 0)
            || (old_size > size && self.seals & linux::F_SEAL_SHRINK != 0)
        {
            bail_libc!(libc::EPERM);
        }

        if old_size != size {
            attr.size = size;
            let now = ctx.now();
            attr.modification_time = now;
            attr.status_change_time = now;
        }

        if old_size <= size {
            return Ok(());
        }

        let old_pgend = offset_page_end(old_size);
        let new_pgend = offset_page_end(size);

        if new_pgend <= old_pgend {
            self.mappings.invalidate(
                MappableRange {
                    start: new_pgend,
                    end: old_pgend,
                },
                InvalidateOpts {
                    invalidate_private: true,
                },
                &ctx.mm(),
            );
        }

        self.data.truncate(size as u64, ctx);
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
        err_libc!(libc::ENOTDIR)
    }

    fn rename(
        &self,
        parents: RenameUnderParents<&mut Inode>,
        old_name: &str,
        new_name: String,
        is_replacement: bool,
        ctx: &dyn Context,
    ) -> SysResult<()> {
        super::rename(parents, old_name, new_name, is_replacement, ctx)
    }

    fn add_link(&self) {
        self.attr.write().unwrap().links += 1;
    }

    fn drop_link(&self) {
        self.attr.write().unwrap().links -= 1;
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl RegularFile {
    pub fn new_file_in_memory(usage: MemoryKind, attr: UnstableAttr) -> Self {
        let ops = FileRangeSetOperations;
        Self {
            attr: RwLock::new(attr),
            mem_usage: usage,
            data: FileRangeSet::new(Box::new(ops)),
            seals: libc::F_SEAL_SEAL,
            mappings: MappingSet::new(Box::new(MappingSetOperations)),
        }
    }

    pub fn write(&mut self, src: &IoSequence, offset: i64, ctx: &dyn Context) -> SysResult<usize> {
        if src.num_bytes() == 0 {
            return Ok(0);
        }
        let now = ctx.now();
        {
            let mut attr = self.attr.write().unwrap();
            attr.modification_time = now;
            attr.status_change_time = now;
        }
        src.copy_in_to(&mut FileReadWriter {
            file: self,
            offset,
            ctx,
        })
    }

    pub fn read(
        &mut self,
        mflags: MountSourceFlags,
        dst: &IoSequence,
        offset: i64,
        ctx: &dyn Context,
    ) -> SysResult<usize> {
        if dst.num_bytes() == 0 {
            return Ok(0);
        }
        {
            let size = self.attr.read().unwrap().size;
            if offset >= size {
                return Err(SysError::eof());
            }
        }
        let ret = dst.copy_out_from(&mut FileReadWriter {
            file: self,
            offset,
            ctx,
        });
        if !mflags.no_atime {
            self.attr.write().unwrap().access_time = ctx.now();
        }
        ret
    }
}

struct FileReadWriter<'a> {
    file: &'a mut RegularFile,
    offset: i64,
    ctx: &'a dyn Context,
}

impl io::Reader for FileReadWriter<'_> {
    fn read_to_blocks(&mut self, mut dsts: BlockSeqView) -> SysResult<usize> {
        let attr_size = self.file.attr.read().unwrap().size;
        if self.offset >= attr_size {
            return Ok(0);
        }
        let end = read_end_offset(self.offset, dsts.num_bytes() as i64, attr_size);
        if end == self.offset {
            return Ok(0);
        }

        let mut done = 0;
        let data = &self.file.data;
        let (mut seg, mut gap) = (
            data.find_segment(self.offset as u64),
            data.find_gap(self.offset as u64),
        );
        while self.offset < end {
            let mr = MappableRange {
                start: self.offset as u64,
                end: end as u64,
            };
            if let Some(seg_inner) = seg {
                let fr = data.file_range_of(&seg_inner, seg_inner.range().intersect(&mr));
                let ims = {
                    let mut mf = self.ctx.memory_file_provider().memory_file_write_lock();
                    mf.map_internal(fr, AccessType::read())?
                };
                let n = copy_seq(dsts, ims.as_view())?;
                done += n;
                self.offset += n as i64;
                dsts.drop_first(n as u64);
                match data.next_non_empty(&seg_inner) {
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
                let g = gap_inner.range().intersect(&mr);
                let dst = dsts.take_first(g.len());
                let n = zero_seq(dst)?;
                done += n;
                self.offset += n as i64;
                dsts.drop_first(n as u64);
                seg = data.next_segment_of_gap(&gap_inner);
                gap = None;
            }
        }
        Ok(done)
    }
}

impl io::Writer for FileReadWriter<'_> {
    fn write_from_blocks(&mut self, mut srcs: BlockSeqView) -> SysResult<usize> {
        if srcs.num_bytes() == 0 {
            return Ok(0);
        }

        let end = write_end_offset(self.offset, srcs.num_bytes() as i64);
        if end == i64::MAX {
            bail_libc!(libc::EINVAL);
        }

        let mut file_attr = self.file.attr.write().unwrap();

        if self.file.seals & linux::F_SEAL_WRITE != 0 {
            bail_libc!(libc::EPERM);
        } else if end > file_attr.size && self.file.seals & linux::F_SEAL_GROW != 0 {
            let pgstart = Addr(file_attr.size as u64).round_down().0 as i64;
            let end = min(end, pgstart);
            if end <= self.offset {
                bail_libc!(libc::EPERM);
            }
        }

        let pgstartaddr = Addr(self.offset as u64).round_down();
        let pgendaddr = Addr(end as u64).round_up().unwrap();
        let pgmr = MappableRange {
            start: pgstartaddr.0,
            end: pgendaddr.0,
        };

        let mut done = 0;
        let (mut seg, mut gap) = {
            let data = &self.file.data;
            (
                data.find_segment(self.offset as u64),
                data.find_gap(self.offset as u64),
            )
        };

        while self.offset < end {
            let mr = MappableRange {
                start: self.offset as u64,
                end: end as u64,
            };
            if let Some(seg_inner) = seg {
                let fr = self
                    .file
                    .data
                    .file_range_of(&seg_inner, seg_inner.range().intersect(&mr));
                let ims = {
                    let mut mf = self.ctx.memory_file_provider().memory_file_write_lock();
                    mf.map_internal(fr, AccessType::write()).map_err(|e| {
                        file_attr.size = max(file_attr.size, self.offset);
                        e
                    })?
                };

                let n = copy_seq(ims.as_view(), srcs).map_err(|e| {
                    file_attr.size = max(file_attr.size, self.offset);
                    e
                })?;
                done += n;
                self.offset += n as i64;
                srcs.drop_first(n as u64);
                match self.file.data.next_non_empty(&seg_inner) {
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
                let g = gap_inner.range().intersect(&pgmr);
                let fr = {
                    let mut mf = self.ctx.memory_file_provider().memory_file_write_lock();
                    mf.allocate(
                        g.len(),
                        AllocOpts {
                            kind: self.file.mem_usage,
                            dir: Direction::BottomUp,
                        },
                    )
                    .map_err(|e| {
                        file_attr.size = max(file_attr.size, self.offset);
                        e
                    })?
                };
                seg = Some(self.file.data.insert(g, fr.start));
                gap = None;
            }
        }
        file_attr.size = max(file_attr.size, self.offset);
        Ok(done)
    }
}

#[derive(Debug)]
pub struct RegularFileOperations {
    pub dirent: DirentRef,
}

impl FileOperations for RegularFileOperations {
    fn dirent(&self) -> DirentRef {
        self.dirent.clone()
    }

    fn read(
        &self,
        _: FileFlags,
        dst: &mut IoSequence,
        offset: i64,
        ctx: &dyn Context,
    ) -> SysResult<usize> {
        let mut dirent = self.dirent.borrow_mut();
        let inode = dirent.inode_mut();
        let mflags = inode.mount_source().flags();
        let iops = inode.inode_operations_mut::<RegularFile>();
        iops.read(mflags, dst, offset, ctx)
    }

    fn write(
        &self,
        _: FileFlags,
        src: &mut IoSequence,
        offset: i64,
        ctx: &dyn Context,
    ) -> SysResult<usize> {
        let mut dirent = self.dirent.borrow_mut();
        let iops = dirent.inode_mut().inode_operations_mut::<RegularFile>();
        iops.write(src, offset, ctx)
    }

    fn configure_mmap(&mut self, opts: &mut MmapOpts) -> SysResult<()> {
        if opts.offset + opts.length > i64::MAX as u64 {
            bail_libc!(libc::EOVERFLOW);
        }
        opts.mappable = Some(self.dirent.clone());
        Ok(())
    }

    fn flush(&self) -> SysResult<()> {
        Ok(())
    }

    fn close(&self) -> SysResult<()> {
        Ok(())
    }

    fn ioctl(&self, _: &libc::user_regs_struct, _: &dyn Context) -> SysResult<usize> {
        bail_libc!(libc::ENOTTY)
    }

    fn seek(
        &mut self,
        inode: &Inode,
        whence: SeekWhence,
        current_offset: i64,
        offset: i64,
    ) -> SysResult<i64> {
        seek_with_dir_cursor(inode, whence, current_offset, offset, None)
    }

    fn readdir(
        &mut self,
        _: i64,
        _: &mut dyn DentrySerializer,
        _: &dyn Context,
    ) -> ReaddirResult<i64> {
        Err(ReaddirError::new(0, libc::ENOTDIR))
    }

    fn readiness(&self, mask: u64, _: &dyn Context) -> u64 {
        mask
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use mem::PAGE_SIZE;

    use super::*;
    use crate::{
        attr::{InodeType, StableAttr},
        mount::{MountSource, MountSourceFlags},
        tmpfs::TMPFS_DEVICE,
        Dirent, TestContext,
    };

    fn new_file_inode(ctx: &dyn Context) -> Inode {
        let m = MountSource::new(MountSourceFlags::default());
        let iops = RegularFile::new_file_in_memory(
            MemoryKind::Tmpfs,
            UnstableAttr::default().record_current_time(|| ctx.now()),
        );
        let tmpfs_device = TMPFS_DEVICE.lock().unwrap();
        Inode::new(
            Box::new(iops),
            Rc::new(m),
            StableAttr {
                device_id: tmpfs_device.device_id(),
                inode_id: tmpfs_device.next_ino(),
                block_size: PAGE_SIZE as i64,
                typ: InodeType::RegularFile,
                device_file_major: 0,
                device_file_minor: 0,
            },
        )
    }

    fn new_file(ctx: &dyn Context) -> File {
        let inode = new_file_inode(ctx);
        let dirent = Dirent::new(inode, "stub".to_string());
        let dirent_ref = dirent.borrow();
        dirent_ref
            .inode()
            .get_file(
                dirent.clone(),
                FileFlags {
                    read: true,
                    write: true,
                    ..FileFlags::default()
                },
            )
            .unwrap()
    }

    #[test]
    fn grow() {
        let ctx = TestContext::init();
        let f = new_file(&ctx);

        let mut abuf = vec![b'a'; 68];
        let mut seq = IoSequence::bytes_sequence(&mut abuf);
        let n = f.pwritev(&mut seq, 0, &ctx);
        assert_eq!(n, Ok(abuf.len()));

        let mut bbuf = vec![b'b'; 856];
        let mut seq = IoSequence::bytes_sequence(&mut bbuf);
        let n = f.pwritev(&mut seq, 68, &ctx);
        assert_eq!(n, Ok(bbuf.len()));

        let mut rbuf = vec![0; abuf.len() + bbuf.len()];
        let n = f.preadv(&mut IoSequence::bytes_sequence(&mut rbuf), 0, &ctx);
        assert_eq!(n, Ok(rbuf.len()));

        let want = {
            abuf.append(&mut bbuf);
            abuf
        };
        assert_eq!(want, rbuf);
    }
}
