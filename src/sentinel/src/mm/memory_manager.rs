use std::{
    cell::RefCell,
    cmp::max,
    collections::HashMap,
    rc::{Rc, Weak},
    sync::RwLock,
};

use arch::{MmapDirection, MmapLayout};
use auth::{user_namespace::UserNamespace, Context as AuthContext};
use limit::Context as LimitContext;
use mem::{
    block::Block,
    block_seq::{copy_seq, zero_seq, BlockSeq, BlockSeqView},
    io::BlockSeqReader,
    AccessType, Addr, AddrRange, AddrRangeSeqView, IoOpts, HUGE_PAGE_SIZE, PAGE_SIZE,
};
use memmap::{
    file::MemmapFile,
    mmap_opts::{MLockMode, MmapOpts},
    InvalidateOpts, Mappable, MappableRange, MemoryInvalidator, Translation,
};
use pgalloc::{AllocOpts, Direction, MemoryFile, MemoryFileProvider};
use platform::PtraceAddressSpace;
use rand::Rng;
use segment::{Gap, Seg, SegOrGap, Set, SetOperations};
use usage::MemoryKind;
use utils::{bail_libc, err_libc, FileRange, Range, SysError, SysResult};

use crate::context;

// Pma represents platform mapping area
#[derive(Clone, Debug)]
struct Pma {
    file: Weak<RwLock<dyn MemmapFile>>,
    off: u64,
    translate_perms: AccessType,
    effective_perms: AccessType,
    max_perms: AccessType,
    internal_mappings: BlockSeq,
    private: bool,
    need_cow: bool,
}

impl PartialEq for Pma {
    fn eq(&self, other: &Self) -> bool {
        self.file.as_ptr() == other.file.as_ptr()
            && self.off == other.off
            && self.translate_perms == other.translate_perms
            && self.effective_perms == other.effective_perms
            && self.max_perms == other.max_perms
            && self.internal_mappings == other.internal_mappings
            && self.private == other.private
            && self.need_cow == other.need_cow
    }
}

impl Eq for Pma {}

type PmaSet = Set<u64, Pma>;

struct PmaSetOperations;
impl SetOperations for PmaSetOperations {
    type K = u64;
    type V = Pma;
    fn merge(
        &self,
        r1: Range<Self::K>,
        v1: &Self::V,
        _r2: Range<Self::K>,
        v2: &Self::V,
    ) -> Option<Self::V> {
        if v1.file.as_ptr() != v2.file.as_ptr()
            || v1.off + r1.len() != v2.off
            || v1.translate_perms != v2.translate_perms
            || v1.effective_perms != v2.effective_perms
            || v1.max_perms != v2.max_perms
            || v1.need_cow != v2.need_cow
            || v1.private != v2.private
        {
            None
        } else {
            let mut ret = v1.clone();
            ret.internal_mappings = BlockSeq::default();
            Some(ret)
        }
    }

    fn split(&self, r: Range<Self::K>, v: &Self::V, split: Self::K) -> (Self::V, Self::V) {
        let newlen1 = split - r.start;
        let mut v = v.clone();
        let mut v2 = v.clone();
        v2.off += newlen1;
        if !v.internal_mappings.is_empty() {
            v.internal_mappings = v.internal_mappings.take_first64(newlen1);
            v2.internal_mappings.drop_first64(newlen1);
        }
        (v, v2)
    }
}

// Vma represents virtual memory area
#[derive(Clone)]
struct Vma {
    mappable: Weak<RefCell<dyn Mappable>>,
    off: u64,
    real_perms: AccessType,
    effective_perms: AccessType,
    max_perms: AccessType,
    private: bool,
    grows_down: bool,
    mlock_mode: MLockMode,
    numa_policy: linux::NumaPolicy,
    numa_nodemask: u64,
}

impl std::fmt::Debug for Vma {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("")
            .field(&self.off)
            .field(&self.real_perms)
            .field(&self.effective_perms)
            .field(&self.max_perms)
            .field(&self.private)
            .field(&self.grows_down)
            .field(&self.mlock_mode)
            .field(&self.numa_policy)
            .finish()
    }
}

impl PartialEq for Vma {
    fn eq(&self, others: &Self) -> bool {
        self.effective_perms == others.effective_perms
            && self.max_perms == others.max_perms
            && self.mappable.as_ptr() == others.mappable.as_ptr()
    }
}

impl Vma {
    fn can_write_mappable(&self) -> bool {
        !self.private && self.max_perms.write
    }

    fn is_private_data(&self) -> bool {
        self.real_perms.write && self.private && !self.grows_down
    }
}

type VmaSet = Set<u64, Vma>;

struct VmaSetOperations;
impl SetOperations for VmaSetOperations {
    type K = u64;
    type V = Vma;
    fn merge(
        &self,
        r1: Range<Self::K>,
        v1: &Self::V,
        _r2: Range<Self::K>,
        v2: &Self::V,
    ) -> Option<Self::V> {
        if v1.mappable.as_ptr() != v2.mappable.as_ptr()
            || (v1.mappable.upgrade().is_some() && v1.off + r1.len() != v2.off)
            || v1.real_perms != v2.real_perms
            || v1.max_perms != v2.max_perms
            || v1.private != v2.private
            || v1.grows_down != v2.grows_down
            || v1.mlock_mode != v2.mlock_mode
            || v1.numa_policy != v2.numa_policy
            || v1.numa_nodemask != v2.numa_nodemask
        {
            None
        } else {
            Some(v1.clone())
        }
    }

    fn split(&self, r: Range<Self::K>, v: &Self::V, split: Self::K) -> (Self::V, Self::V) {
        let mut v2 = v.clone();
        if v2.mappable.upgrade().is_some() {
            v2.off += split - r.start;
        }
        (v.clone(), v2)
    }
}

const MAP32START: u64 = 0x40000000;
const MAP32END: u64 = 0x80000000;
const GUARD_BYTES: i32 = 256 * PAGE_SIZE;

#[derive(PartialEq, Eq)]
pub enum MremapMoveMode {
    No,
    May,
    Must,
}

pub struct MremapOpts {
    pub mov: MremapMoveMode,
    pub new_addr: Addr,
}

#[derive(Debug)]
pub struct MemoryManager {
    layout: MmapLayout,
    pmas: PmaSet,
    vmas: VmaSet,
    brk: AddrRange,
    usage_address_space: u64,
    locked_as: u64,
    data_address_space: u64,
    cur_rss: u64,
    max_rss: u64,
    private_refs: Rc<RefCell<PrivateRefs>>,
    address_space: Option<Box<PtraceAddressSpace>>,
    unmap_all_on_active: bool,
    capture_invalidations: bool,
    def_mlock_mode: MLockMode,
    argv: AddrRange,
    envv: AddrRange,
    auxv: HashMap<u64, Addr>,
}

impl MemoryManager {
    pub fn new() -> Self {
        let pma_ops = PmaSetOperations;
        let vma_ops = VmaSetOperations;
        Self {
            layout: MmapLayout::default(),
            pmas: PmaSet::new(Box::new(pma_ops)),
            vmas: VmaSet::new(Box::new(vma_ops)),
            usage_address_space: 0,
            brk: AddrRange::default(),
            locked_as: 0,
            data_address_space: 0,
            cur_rss: 0,
            max_rss: 0,
            private_refs: Rc::new(RefCell::new(PrivateRefs::new())),
            address_space: None,
            unmap_all_on_active: false,
            capture_invalidations: false,
            def_mlock_mode: MLockMode::default(),
            argv: AddrRange::default(),
            envv: AddrRange::default(),
            auxv: HashMap::new(),
        }
    }

    pub fn set_envv_start(&mut self, a: Addr) {
        self.envv.start = a.0;
    }

    pub fn set_envv_end(&mut self, a: Addr) {
        self.envv.end = a.0;
    }

    pub fn set_argv_start(&mut self, a: Addr) {
        self.argv.start = a.0;
    }

    pub fn set_argv_end(&mut self, a: Addr) {
        self.argv.end = a.0;
    }

    pub fn set_mmap_layout(&mut self) -> SysResult<MmapLayout> {
        let ctx = &*context::context();
        let platform = ctx.platform();
        let layout = MmapLayout::new(
            platform.min_user_address(),
            platform.max_user_address(),
            &ctx.limits(),
        )?;
        self.layout = layout;
        Ok(layout)
    }

    pub fn set_address_space(&mut self, address_space: Option<Box<PtraceAddressSpace>>) {
        self.address_space = address_space;
    }

    pub fn set_auxv(&mut self, auxv: HashMap<u64, Addr>) {
        self.auxv = auxv;
    }

    pub fn check_io_range(&self, addr: Addr, length: i64) -> Option<AddrRange> {
        let range = addr.to_range(length as u64)?;
        if range.end <= self.layout.max_addr.0 as u64 {
            Some(range)
        } else {
            None
        }
    }

    pub fn set_numa_policy(
        &mut self,
        addr: Addr,
        length: u64,
        policy: linux::NumaPolicy,
        nodemask: u64,
    ) -> SysResult<()> {
        if addr.page_offset() != 0 {
            bail_libc!(libc::EINVAL);
        }
        let la = Addr(length)
            .round_up()
            .ok_or_else(|| SysError::new(libc::EINVAL))?;
        let ar = addr
            .to_range(la.0)
            .ok_or_else(|| SysError::new(libc::EINVAL))?;
        if ar.is_empty() {
            return Ok(());
        }
        let mut vseg = self.vmas.lower_bound_segment(ar.start);
        let mut last_end = ar.start;
        loop {
            let v = vseg.ok_or_else(|| SysError::new(libc::EINVAL))?;
            if last_end < v.start() {
                self.vmas.merge_range(ar);
                self.vmas.merge_adjacant(ar);
                bail_libc!(libc::EFAULT);
            }
            let v = self.vmas.isolate(&v, ar);
            let vma = self.vmas.value_mut(&v);
            vma.numa_policy = policy;
            vma.numa_nodemask = nodemask;
            last_end = v.end();
            if ar.end <= last_end {
                self.vmas.merge_range(ar);
                self.vmas.merge_adjacant(ar);
                return Ok(());
            }
            vseg = match self.vmas.next_non_empty(&v) {
                Some(SegOrGap::Segment(v)) => Some(v),
                _ => None,
            };
        }
    }

    fn check_io_vec(&self, mut ars: AddrRangeSeqView) -> bool {
        while !ars.is_empty() {
            let ar = ars.head();
            if self
                .check_io_range(Addr(ar.start), ar.len() as i64)
                .is_none()
            {
                return false;
            }
            ars = ars.tail();
        }
        true
    }

    fn get_pmas(
        &mut self,
        vseg: Seg<u64>,
        mut ar: AddrRange,
        at: AccessType,
    ) -> (Option<Seg<u64>>, Option<Gap<u64>>, SysResult<()>) {
        let (end, alignres) = match Addr(ar.end).round_up() {
            Some(e) => (e, Ok(())),
            None => (Addr(ar.end).round_down(), err_libc!(libc::EFAULT)),
        };
        ar = AddrRange {
            start: Addr(ar.start).round_down().0,
            end: end.0,
        };
        let (mut pstart, pend, perr) = self.get_pmas_internal(vseg, ar, at);
        if pend.map(|e| e.start()).unwrap_or(0) <= ar.start as u64 {
            return (None, pend, perr);
        }
        if pstart.is_none() {
            // NOTE: pend should not be "terminal", so unwrap
            pstart = self.find_or_seek_prev_upper_bound_pma(Addr(ar.start), pend.unwrap());
        }
        if perr.is_err() {
            (pstart, pend, perr)
        } else {
            (pstart, pend, alignres)
        }
    }

    fn get_pmas_internal(
        &mut self,
        mut vseg: Seg<u64>,
        ar: AddrRange,
        at: AccessType,
    ) -> (Option<Seg<u64>>, Option<Gap<u64>>, SysResult<()>) {
        let mask_ar = private_aligned(ar);
        let mut did_unmap_as = false;
        let mut pseg = self.pmas.find_segment(ar.start as u64);
        let mut pgap = self.pmas.find_gap(ar.start as u64);
        let mut pstart = pseg;
        loop {
            let vseg_ar = vseg.range().intersect(&ar);
            let vma = self.vmas.value(&vseg);
            let vma_effective_perms = vma.effective_perms;
            let vma_max_perms = vma.max_perms;
            let vma_private = vma.private;
            let vma_mappable = vma.mappable.clone();
            let vma_grows_down = vma.grows_down;
            'pma_loop: loop {
                if pgap.map(|p| p.start() < vseg_ar.end).unwrap_or(false) {
                    let opt_ar = vseg.range().intersect(&pgap.unwrap().range());
                    match vma_mappable.upgrade() {
                        Some(ref vma_mappable) => {
                            let opt_mr = self.mappable_range_of(opt_ar, &vseg);
                            let req_ar = opt_ar.intersect(&ar);
                            let req_mr = self.mappable_range_of(req_ar, &vseg);
                            let mut perms = at;
                            if vma_private {
                                perms.read = true;
                                perms.write = false;
                            }
                            let (ts, res) =
                                vma_mappable.borrow_mut().translate(req_mr, opt_mr, perms);
                            if ts.is_empty() {
                                return (pstart, pgap, res);
                            }
                            pstart = None;
                            for t in &ts {
                                let newpma_ar = self.addr_range_of(t.source(), &vseg);
                                let mut newpma = Pma {
                                    file: t.file().clone(),
                                    off: t.offset(),
                                    translate_perms: t.perms(),
                                    effective_perms: vma_effective_perms.intersect(t.perms()),
                                    max_perms: vma_max_perms.intersect(t.perms()),
                                    private: false,
                                    internal_mappings: BlockSeq::default(),
                                    need_cow: false,
                                };
                                if vma_private {
                                    newpma.effective_perms.write = false;
                                    newpma.max_perms.write = false;
                                    newpma.need_cow = true;
                                }
                                self.add_rss(newpma_ar);
                                pseg = Some(self.pmas.insert(newpma_ar, newpma));
                                pgap = self.pmas.next_gap_of_seg(&pseg.unwrap());
                            }
                            if res.is_err()
                                && self.addr_range_of(ts.last().unwrap().source(), &vseg).end
                                    < ar.end
                            {
                                return (pstart, pgap, res);
                            }
                            pseg = self.find_or_seek_prev_upper_bound_pma(
                                Addr(self.addr_range_of(ts[0].source(), &vseg).start),
                                pgap.expect("pgap should not be terminal iterator"),
                            );
                            pgap = None;
                            continue;
                        }
                        None => {
                            let alloc_ar = opt_ar.intersect(&mask_ar);
                            let ctx = context::context();
                            let mut mf = ctx.kernel().memory_file_write_lock();
                            let fr = match mf.allocate(
                                alloc_ar.len(),
                                AllocOpts {
                                    kind: MemoryKind::Anonymous,
                                    dir: Direction::BottomUp,
                                },
                            ) {
                                Ok(fr) => fr,
                                Err(err) => return (pstart, pgap, Err(err)),
                            };
                            self.add_rss(alloc_ar);
                            let inserted = self.pmas.insert(
                                alloc_ar,
                                Pma {
                                    file: Rc::<RwLock<MemoryFile>>::downgrade(
                                        ctx.kernel().memory_file(),
                                    ),
                                    off: fr.start,
                                    translate_perms: AccessType::any_access(),
                                    effective_perms: vma_effective_perms,
                                    max_perms: vma_max_perms,
                                    private: true,
                                    internal_mappings: BlockSeq::default(),
                                    need_cow: false,
                                },
                            );
                            match self.pmas.next_non_empty(&inserted) {
                                Some(SegOrGap::Segment(s)) => {
                                    pseg = Some(s);
                                    pgap = None;
                                }
                                Some(SegOrGap::Gap(g)) => {
                                    pseg = None;
                                    pgap = Some(g);
                                }
                                None => {
                                    pseg = None;
                                    pgap = None;
                                }
                            };
                            pstart = None;
                        }
                    }
                } else if pseg.map(|p| p.start() < vseg_ar.end).unwrap_or(false) {
                    let unwrapped_pseg = pseg.unwrap();
                    if at.write && self.pma_copy_on_write(&vseg, &unwrapped_pseg) {
                        let copy_ar = if self.vmas.value(&vseg).effective_perms.execute {
                            unwrapped_pseg.range().intersect(&ar)
                        } else if vma_grows_down {
                            let stack_mask_ar = AddrRange {
                                start: ar.start.checked_sub(PAGE_SIZE as u64).unwrap_or(ar.start),
                                end: ar.end.checked_add(PAGE_SIZE as u64).unwrap_or(ar.end),
                            };
                            unwrapped_pseg.range().intersect(&stack_mask_ar)
                        } else {
                            unwrapped_pseg.range().intersect(&mask_ar)
                        };
                        if let Err(e) = self.get_internal_mappings(unwrapped_pseg) {
                            return (pstart, self.pmas.prev_gap_of_seg(&unwrapped_pseg), Err(e));
                        }

                        let fr = {
                            let r = BlockSeqReader {
                                src: self.internal_mappings(unwrapped_pseg, copy_ar),
                            };
                            let ctx = context::context();
                            let mut mf = ctx.kernel().memory_file_write_lock();
                            mf.allocate_and_fill(copy_ar.len(), MemoryKind::Anonymous, r)
                        };
                        let fr = match fr {
                            Ok(n) => n,
                            Err(err) => {
                                return (
                                    pstart,
                                    self.pmas.prev_gap_of_seg(&unwrapped_pseg),
                                    Err(err),
                                )
                            }
                        };
                        if fr.is_empty() {
                            return (pstart, self.pmas.prev_gap_of_seg(&unwrapped_pseg), Ok(()));
                        }
                        if !did_unmap_as {
                            self.unmap_address_space(mask_ar);
                            did_unmap_as = true;
                        }
                        let copy_ar = AddrRange {
                            start: copy_ar.start,
                            end: copy_ar.start + fr.len(),
                        };
                        if copy_ar != unwrapped_pseg.range() {
                            pseg = Some(self.pmas.isolate(&unwrapped_pseg, copy_ar));
                            pstart = None;
                        }
                        let mut unwrapped_pseg = pseg.unwrap();
                        let old_pma = self.pmas.value_mut(&unwrapped_pseg);
                        let ctx = context::context();
                        old_pma.file =
                            Rc::<RwLock<MemoryFile>>::downgrade(ctx.kernel().memory_file());
                        old_pma.off = fr.start;
                        old_pma.translate_perms = AccessType::any_access();
                        old_pma.effective_perms = vma_effective_perms;
                        old_pma.max_perms = vma_max_perms;
                        old_pma.need_cow = false;
                        old_pma.private = true;
                        old_pma.internal_mappings = BlockSeq::default();
                        if let Some(prev) = self.pmas.prev_segment_of_seg(&unwrapped_pseg) {
                            if let Some(merged) = self.pmas.merge(prev, unwrapped_pseg) {
                                pseg = Some(merged);
                                unwrapped_pseg = merged;
                                pstart = None;
                            }
                        }
                        if let Some(next) = self.pmas.next_segment_of_seg(&unwrapped_pseg) {
                            if let Some(merged) = self.pmas.merge(unwrapped_pseg, next) {
                                pseg = Some(merged);
                                unwrapped_pseg = merged;
                                pstart = None;
                            }
                        }
                        if pseg.map_or(false, |s| s.end() < ar.end) {
                            return (pstart, self.pmas.next_gap_of_seg(&unwrapped_pseg), Ok(()));
                        }
                        match self.pmas.next_non_empty(&unwrapped_pseg) {
                            Some(SegOrGap::Gap(g)) => {
                                pseg = None;
                                pgap = Some(g);
                            }
                            Some(SegOrGap::Segment(s)) => {
                                pseg = Some(s);
                                pgap = None;
                            }
                            None => {
                                pseg = None;
                                pgap = None;
                            }
                        }
                    } else if !self
                        .pmas
                        .value(&unwrapped_pseg)
                        .translate_perms
                        .is_superset_of(at)
                    {
                        let old_pma = self.pmas.value(&unwrapped_pseg);
                        let opt_ar = unwrapped_pseg.range();
                        let opt_mr = self.mappable_range_of(opt_ar, &vseg);
                        let req_ar = opt_ar.intersect(&ar);
                        let req_mr = self.mappable_range_of(req_ar, &vseg);
                        let perms = old_pma.translate_perms.union(at);
                        let (ts, res) = vma_mappable
                            .upgrade()
                            .unwrap()
                            .borrow_mut()
                            .translate(req_mr, opt_mr, perms);
                        if ts.is_empty() {
                            return (pstart, self.pmas.prev_gap_of_seg(&unwrapped_pseg), res);
                        }
                        let trans_mr = MappableRange {
                            start: ts.first().unwrap().source().start,
                            end: ts.last().unwrap().source().end,
                        };
                        let trans_ar = self.addr_range_of(trans_mr, &vseg);
                        let mut pseg_inner = self.pmas.isolate(&unwrapped_pseg, trans_ar);
                        pgap = Some(self.pmas.remove(pseg_inner.range()));
                        pstart = None;
                        for t in &ts {
                            let new_pma_ar = self.addr_range_of(t.source(), &vseg);
                            let mut new_pma = Pma {
                                file: t.file().clone(),
                                off: t.offset(),
                                translate_perms: t.perms(),
                                effective_perms: vma_effective_perms.intersect(t.perms()),
                                max_perms: vma_max_perms.intersect(t.perms()),
                                internal_mappings: BlockSeq::default(),
                                private: false,
                                need_cow: false,
                            };
                            if vma_private {
                                new_pma.effective_perms.write = false;
                                new_pma.max_perms.write = false;
                                new_pma.need_cow = true;
                            }
                            pseg_inner = self.pmas.insert(new_pma_ar, new_pma);
                            pgap = self.pmas.next_gap_of_seg(&pseg_inner);
                        }
                        if res.is_err() && pseg_inner.end() < ar.end {
                            return (pstart, pgap, res);
                        }
                        let r = pgap.unwrap().range();
                        if r.start == r.end {
                            pseg = self.pmas.next_segment_of_gap(&pgap.unwrap());
                            pgap = None;
                        } else {
                            pseg = None;
                        }
                    } else {
                        match self.pmas.next_non_empty(&unwrapped_pseg) {
                            Some(SegOrGap::Gap(g)) => {
                                pseg = None;
                                pgap = Some(g);
                            }
                            Some(SegOrGap::Segment(s)) => {
                                pseg = Some(s);
                                pgap = None;
                            }
                            None => {
                                pseg = None;
                                pgap = None;
                            }
                        }
                    }
                } else {
                    break 'pma_loop;
                }
            }
            if ar.end <= vseg.end() {
                if pgap.is_some() {
                    return (pstart, pgap, Ok(()));
                } else {
                    return (pstart, self.pmas.prev_gap_of_seg(&pseg.unwrap()), Ok(()));
                }
            }
            vseg = self.vmas.next_segment_of_seg(&vseg).unwrap(); // TODO: handle vseg None?
        }
    }

    fn mappable_range_of(&self, r: Range<u64>, vseg: &Seg<u64>) -> MappableRange {
        let vma = self.vmas.value(vseg);
        let vstart = vseg.start();
        MappableRange {
            start: vma.off + (r.start - vstart),
            end: vma.off + (r.end - vstart),
        }
    }

    fn mappable_offset_at(&self, addr: Addr, vseg: &Seg<u64>) -> u64 {
        let vma = self.vmas.value(vseg);
        let vstart = vseg.start();
        vma.off + (addr.0 - vstart)
    }

    fn addr_range_of(&self, mr: MappableRange, vseg: &Seg<u64>) -> AddrRange {
        let vma = self.vmas.value(vseg);
        let vstart = vseg.start();
        AddrRange {
            start: vstart + (mr.start - vma.off),
            end: vstart + (mr.end - vma.off),
        }
    }

    fn pma_copy_on_write(&mut self, vseg: &Seg<u64>, pseg: &Seg<u64>) -> bool {
        let pma = self.pmas.value(pseg);
        if !pma.need_cow {
            return false;
        }
        if !pma.private {
            return true;
        }
        let fr = self.file_range(*pseg);
        let refs_set = &self.private_refs.as_ref().borrow().refs;
        let rseg = refs_set.find_segment(fr.start);
        if rseg.map_or(false, |r| *refs_set.value(&r) == 1 && fr.end <= r.end()) {
            let pma = self.pmas.value_mut(pseg);
            pma.need_cow = false;
            let vma = self.vmas.value(vseg);
            pma.effective_perms = vma.effective_perms;
            pma.max_perms = vma.max_perms;
            false
        } else {
            true
        }
    }

    fn get_pma_internal_mappings(
        &mut self,
        mut pseg: Seg<u64>,
        ar: AddrRange,
    ) -> (Option<Gap<u64>>, SysResult<()>) {
        loop {
            if let Err(e) = self.get_internal_mappings(pseg) {
                return (self.pmas.prev_gap_of_seg(&pseg), Err(e));
            }
            if ar.end as u64 <= pseg.end() {
                return (self.pmas.next_gap_of_seg(&pseg), Ok(()));
            }
            // FIXME(bad implementation)
            pseg = match self.pmas.next_non_empty(&pseg) {
                Some(SegOrGap::Segment(s)) => s,
                _ => return (None, Ok(())),
            };
        }
    }

    fn get_vec_pmas<'a>(
        &mut self,
        ars: AddrRangeSeqView<'a>,
        at: AccessType,
    ) -> (AddrRangeSeqView<'a>, SysResult<()>) {
        let mut arsit = ars;
        while !arsit.is_empty() {
            let mut ar = arsit.head();
            if ar.is_empty() {
                continue;
            }
            let (end, alignres) = match Addr(ar.end).round_up() {
                Some(e) => (e, Ok(())),
                None => (Addr(ar.end).round_down(), err_libc!(libc::EFAULT)),
            };
            ar = AddrRange {
                start: Addr(ar.start).round_down().0,
                end: end.0,
            };
            let seg = self.vmas.find_segment(ar.start as u64);
            let (_, pend, pres) = self.get_pmas_internal(seg.unwrap(), ar, at);
            let pstart = pend.map(|p| p.start()).unwrap_or(0);
            if pres.is_err() {
                return (truncated_addr_range_seq(ars, arsit, pstart), pres);
            }
            if alignres.is_err() {
                return (truncated_addr_range_seq(ars, arsit, pstart), alignres);
            }
            arsit = arsit.tail();
        }
        (ars, Ok(()))
    }

    fn get_vec_vmas<'a>(
        &self,
        ars: AddrRangeSeqView<'a>,
        at: AccessType,
        ignore_permissions: bool,
    ) -> (AddrRangeSeqView<'a>, SysResult<()>) {
        let mut arsit = ars;
        while !arsit.is_empty() {
            let ar = arsit.head();
            if ar.is_empty() {
                continue;
            }
            if let (_, vend, Err(err)) = self.get_vmas(ar, at, ignore_permissions) {
                return (
                    truncated_addr_range_seq(
                        ars,
                        arsit,
                        vend.map(|k| k.start()).unwrap_or(u64::MIN),
                    ),
                    Err(err),
                );
            }
            arsit = arsit.tail();
        }
        (ars, Ok(()))
    }

    // get_vmas ensures that vmas exist for all address in ar
    fn get_vmas(
        &self,
        ar: AddrRange,
        at: AccessType,
        ignore_permissions: bool,
    ) -> (Option<Seg<u64>>, Option<Gap<u64>>, SysResult<()>) {
        debug_assert!(ar.is_well_formed());
        debug_assert!(!ar.is_empty());

        let mut addr = ar.start;
        let mut vbegin = self.vmas.find_segment(addr);
        let mut vgap = self.vmas.find_gap(addr);
        if let Some(vbegin) = vbegin {
            vgap = self.vmas.prev_gap_of_seg(&vbegin);
        } else {
            vbegin = self
                .vmas
                .next_segment_of_gap(&vgap.expect("either vbegin or vgap should be something"));
        }

        let mut vseg_maybe = vbegin;
        while let Some(vseg) = vseg_maybe {
            if addr < vseg.start() {
                return (vbegin, vgap, err_libc!(libc::EFAULT));
            }
            let vma = self.vmas.value(&vseg);
            let perms = if ignore_permissions {
                vma.max_perms
            } else {
                vma.effective_perms
            };
            if !perms.is_superset_of(at) {
                return (vbegin, vgap, err_libc!(libc::EPERM));
            }
            addr = vseg.end();
            vgap = self.vmas.next_gap_of_seg(&vseg);
            if addr >= ar.end {
                return (vbegin, vgap, Ok(()));
            }
            vseg_maybe = vgap.and_then(|v| self.vmas.next_segment_of_gap(&v));
        }
        (vbegin, vgap, err_libc!(libc::EFAULT))
    }

    fn with_vec_internal_mappings<F: FnMut(BlockSeqView) -> SysResult<usize>>(
        &mut self,
        ars: AddrRangeSeqView,
        at: AccessType,
        ignore_permissions: bool,
        mut f: F,
    ) -> SysResult<usize> {
        if ars.num_ranges() == 1 {
            return self.with_internal_mappings(ars.head(), at, ignore_permissions, f);
        }
        if self.existing_vec_pmas(ars, at, ignore_permissions, true) {
            return f(self.vec_internal_mappings(ars).as_view());
        }

        let (vars, vres) = self.get_vec_vmas(ars, at, ignore_permissions);
        if vars.num_bytes() == 0 {
            vres.map_err(translate_io_error)?;
            return Ok(0);
        }

        let (pars, pres) = self.get_vec_pmas(vars, at);
        if pars.num_bytes() == 0 {
            pres.map_err(translate_io_error)?;
            return Ok(0);
        }

        let (imars, imres) = self.get_vec_pma_internal_mappings(pars);
        if imars.num_bytes() == 0 {
            imres.map_err(translate_io_error)?;
            return Ok(0);
        }

        let n = f(self.vec_internal_mappings(imars).as_view()).map_err(translate_io_error)?;
        imres.map_err(translate_io_error)?;
        pres.map_err(translate_io_error)?;
        vres.map_err(translate_io_error)?;
        Ok(n)
    }

    fn get_vec_pma_internal_mappings<'a>(
        &mut self,
        ars: AddrRangeSeqView<'a>,
    ) -> (AddrRangeSeqView<'a>, SysResult<()>) {
        let mut arsit = ars;
        while !arsit.is_empty() {
            let ar = arsit.head();
            if ar.is_empty() {
                continue;
            }
            let seg = self.pmas.find_segment(ar.start).unwrap();
            let (pend, res) = self.get_pma_internal_mappings(seg, ar);
            if res.is_err() {
                return (
                    truncated_addr_range_seq(ars, arsit, pend.unwrap().start()),
                    res,
                );
            }
            arsit = arsit.tail();
        }
        (ars, Ok(()))
    }

    fn vec_internal_mappings(&self, mut ars: AddrRangeSeqView) -> BlockSeq {
        let mut blocks = Vec::new();
        while !ars.is_empty() {
            let ar = ars.head();
            if ar.is_empty() {
                continue;
            }
            let s = self.pmas.find_segment(ar.start as u64).unwrap();
            let mut pims = self.internal_mappings(s, ar);
            while !pims.is_empty() {
                blocks.push(pims.head());
                pims = pims.tail();
            }
            ars = ars.tail();
        }
        BlockSeq::from_blocks(blocks)
    }

    fn internal_mappings(&self, mut pseg: Seg<u64>, ar: AddrRange) -> BlockSeq {
        if ar.end <= pseg.end() {
            let offset = ar.start - pseg.start();
            return self
                .pmas
                .value(&pseg)
                .internal_mappings
                .cut_first(offset)
                .take_first64(ar.len());
        }

        let mut blocks = Vec::new();
        loop {
            let pr = pseg.range().intersect(&ar);
            let mut pims = self
                .pmas
                .value(&pseg)
                .internal_mappings
                .cut_first(pr.start - pseg.start())
                .take_first64(pr.len());
            while !pims.is_empty() {
                blocks.push(pims.head());
                pims = pims.tail();
            }
            if ar.end <= pseg.end() {
                break;
            }
            pseg = self.pmas.next_segment_of_seg(&pseg).unwrap(); // ar.1 > pseg.end() is guaranteed
        }
        BlockSeq::from_blocks(blocks)
    }

    fn with_internal_mappings<F: FnMut(BlockSeqView) -> SysResult<usize>>(
        &mut self,
        mut ar: AddrRange,
        at: AccessType,
        ignore_permissions: bool,
        mut f: F,
    ) -> SysResult<usize> {
        if let Some(pseg) = self.existing_pmas(ar, at, ignore_permissions, true) {
            return f(self.internal_mappings(pseg, ar).as_view());
        }
        let (vseg, vend, vres) = self.get_vmas(ar, at, ignore_permissions);
        {
            let vend_addr = vend.map_or(0, |v| v.start());
            if vend_addr < ar.end as u64 {
                if vend_addr <= ar.start as u64 {
                    vres.map_err(translate_io_error)?;
                    return Ok(0);
                }
                ar.end = vend_addr;
            }
        }
        // NOTE: vseg should be valid
        let (pseg, pend, pres) = self.get_pmas(vseg.expect("terminal iterator"), ar, at);
        {
            let pend_addr = pend.map_or(0, |p| p.start());
            if pend_addr < ar.end as u64 {
                if pend_addr <= ar.start as u64 {
                    pres.map_err(translate_io_error)?;
                    return Ok(0);
                }
                ar.end = pend_addr;
            }
        }
        // Note: pseg should contain ar, which means non-terminal.
        let (imend, imres) = self.get_pma_internal_mappings(pseg.unwrap(), ar);
        {
            let imend_addr = imend.map_or(0, |i| i.start());
            if imend_addr < ar.end as u64 {
                if imend_addr <= ar.start as u64 {
                    imres.map_err(translate_io_error)?;
                    return Ok(0);
                }
                ar.end = imend_addr;
            }
        }

        // Note: pseg should contain ar, which means non-terminal.
        let n = f(self.internal_mappings(pseg.unwrap(), ar).as_view())?;
        imres.map_err(translate_io_error)?;
        pres.map_err(translate_io_error)?;
        vres.map_err(translate_io_error)?;
        Ok(n)
    }

    fn existing_vec_pmas(
        &self,
        mut ars: AddrRangeSeqView,
        at: AccessType,
        ignore_permissions: bool,
        need_internal_mappings: bool,
    ) -> bool {
        while !ars.is_empty() {
            let ar = ars.head();
            if !ar.is_empty()
                && self
                    .existing_pmas(ar, at, ignore_permissions, need_internal_mappings)
                    .is_none()
            {
                return false;
            }
            ars = ars.tail();
        }
        true
    }

    fn existing_pmas(
        &self,
        ar: AddrRange,
        at: AccessType,
        ignore_permissions: bool,
        need_internal_mappings: bool,
    ) -> Option<Seg<u64>> {
        let first = self.pmas.find_segment(ar.start);
        let mut pseg_maybe = first;
        while let Some(pseg) = pseg_maybe {
            let pma = self.pmas.value(&pseg);
            let perms = if ignore_permissions {
                pma.max_perms
            } else {
                pma.effective_perms
            };
            if !perms.is_superset_of(at) {
                return None;
            }
            if need_internal_mappings && pma.internal_mappings.is_empty() {
                return None;
            }
            if ar.end <= pseg.end() {
                return first;
            }
            pseg_maybe = match self.pmas.next_non_empty(&pseg) {
                Some(SegOrGap::Segment(seg)) => Some(seg),
                _ => None,
            };
        }
        None
    }

    fn add_rss(&mut self, ar: AddrRange) {
        self.cur_rss += ar.len() as u64;
        self.max_rss = max(self.max_rss, self.cur_rss);
    }

    fn remove_rss(&mut self, r: Range<u64>) {
        self.cur_rss -= r.end - r.start;
    }

    fn find_or_seek_prev_upper_bound_pma(&self, addr: Addr, pgap: Gap<u64>) -> Option<Seg<u64>> {
        debug_assert!(addr.0 <= pgap.start());
        let pseg = self.pmas.prev_segment_of_gap(&pgap);
        if pseg.map(|s| s.start()).unwrap_or(0) <= addr.0 as u64 {
            pseg
        } else {
            self.pmas.upper_bound_segment(addr.0 as u64)
        }
    }

    fn get_internal_mappings(&mut self, pseg: Seg<u64>) -> SysResult<()> {
        let fr = self.file_range(pseg);
        let pma = self.pmas.value_mut(&pseg);
        if pma.internal_mappings.is_empty() {
            let mut perms = pma.max_perms;
            perms.execute = false;
            let ims = pma
                .file
                .upgrade()
                .unwrap()
                .write()
                .unwrap()
                .map_internal(fr, perms)?;
            pma.internal_mappings = ims;
        }
        Ok(())
    }

    fn file_range(&self, pseg: Seg<u64>) -> FileRange {
        let r = pseg.range();
        let pma = self.pmas.value(&pseg);
        let pstart = pseg.start();
        FileRange {
            start: pma.off + (r.start - pstart),
            end: pma.off + (r.end - pstart),
        }
    }

    fn file_range_of(&self, pseg: &Seg<u64>, ar: AddrRange) -> FileRange {
        let pma = self.pmas.value(pseg);
        let pstart = pseg.start();
        FileRange {
            start: pma.off + (ar.start as u64 - pstart),
            end: pma.off + (ar.end as u64 - pstart),
        }
    }

    fn unmap_address_space(&mut self, ar: AddrRange) {
        let ar = ar.intersect(&self.application_addr_range());
        let ctx = &*context::context();
        match self.address_space {
            Some(ref mut address_space) => {
                address_space.unmap(Addr(ar.start), ar.len(), ctx);
            }
            None => self.unmap_all_on_active = true,
        }
    }

    fn application_addr_range(&self) -> AddrRange {
        AddrRange {
            start: self.layout.min_addr.0 as u64,
            end: self.layout.max_addr.0 as u64,
        }
    }

    pub fn handle_user_fault(&mut self, addr: Addr, at: AccessType) -> SysResult<()> {
        let ar = addr
            .round_down()
            .to_range(PAGE_SIZE as u64)
            .ok_or_else(|| SysError::new(libc::EFAULT))?;
        let (vseg, _, res) = self.get_vmas(ar, at, false);
        res?;
        let (pseg, _, res) = self.get_pmas(vseg.unwrap(), ar, at);
        res?;
        self.map_address_space(pseg.unwrap(), ar, false)
    }

    pub fn mmap(&mut self, mut opts: MmapOpts) -> SysResult<Addr> {
        if opts.length == 0 {
            bail_libc!(libc::EINVAL);
        }
        let length = Addr(opts.length)
            .round_up()
            .ok_or_else(|| SysError::new(libc::ENOMEM))?;
        opts.length = length.0;

        if opts.mappable.is_some() {
            if Addr(opts.offset).round_down() != Addr(opts.offset) {
                bail_libc!(libc::ENOMEM);
            }
            opts.offset
                .checked_add(opts.length)
                .ok_or_else(|| SysError::new(libc::ENOMEM))?;
        } else {
            opts.offset = 0;
        }

        if opts.addr.round_down() != opts.addr {
            if opts.fixed {
                bail_libc!(libc::EINVAL);
            }
            opts.addr = opts.addr.round_down();
        }

        if !opts.max_perms.is_superset_of(opts.perms) {
            bail_libc!(libc::EACCES);
        }
        if opts.unmap && !opts.fixed {
            bail_libc!(libc::EINVAL);
        }
        if opts.grows_down && opts.mappable.is_some() {
            bail_libc!(libc::EINVAL);
        }

        if opts.mlock_mode < self.def_mlock_mode {
            opts.mlock_mode = self.def_mlock_mode;
        }
        let (vseg, ar) = self.create_vma(&opts)?;
        if opts.precommit || opts.mlock_mode == MLockMode::Eager {
            self.populate_vma(&vseg, ar, true);
        } else if opts.mappable.is_none() && length.0 <= PRIVATE_ALLOC_UNIT {
            self.populate_vma(&vseg, ar, false);
        }
        Ok(Addr(ar.start))
    }

    pub fn munmap(&mut self, addr: Addr, length: u64) -> SysResult<()> {
        if addr != addr.round_down() {
            bail_libc!(libc::EINVAL);
        }
        if length == 0 {
            bail_libc!(libc::EINVAL);
        }
        let la = Addr(length)
            .round_up()
            .ok_or_else(|| SysError::new(libc::EINVAL))?;
        let ar = addr
            .to_range(la.0)
            .ok_or_else(|| SysError::new(libc::EINVAL))?;
        self.unmap(ar);
        Ok(())
    }

    pub fn mremap(
        &mut self,
        old_addr: Addr,
        old_size: u64,
        new_size: u64,
        opts: &MremapOpts,
    ) -> SysResult<Addr> {
        if old_addr.round_down() != old_addr {
            bail_libc!(libc::EINVAL);
        }

        let old_size_addr = Addr(old_size).round_up().expect("should handle None");
        let old_size = old_size_addr.0;
        let new_size = match Addr(new_size).round_up() {
            Some(Addr(0)) | None => bail_libc!(libc::EINVAL),
            Some(addr) => addr,
        }
        .0;
        let mut old_end = old_addr
            .add_length(old_size)
            .ok_or_else(|| SysError::new(libc::EINVAL))?;
        let mut vseg = self
            .vmas
            .find_segment(old_addr.0)
            .ok_or_else(|| SysError::new(libc::EFAULT))?;
        let vma = self.vmas.value(&vseg);
        if new_size > old_size && vma.mlock_mode != MLockMode::None_ {
            let ctx = context::context();
            let mlock_limit = ctx.limits().get_memory_locked().cur;
            let creds = ctx.credentials();
            if !creds.has_capability_in(
                &linux::Capability::ipc_lock(),
                UserNamespace::get_root(&creds.user_namespace),
            ) {
                let new_locked_as = self.locked_as - old_size + new_size;
                if new_locked_as > mlock_limit {
                    bail_libc!(libc::EAGAIN)
                }
            }
        }

        if opts.mov != MremapMoveMode::Must {
            if new_size <= old_size {
                if new_size < old_size {
                    let new_end = old_addr.0 + new_size;
                    self.unmap(AddrRange {
                        start: new_end,
                        end: old_end.0,
                    });
                }
                return Ok(old_addr);
            }

            if vseg.end() < old_end.0 {
                bail_libc!(libc::EFAULT);
            }
            let vma = self.vmas.value(&vseg).clone();
            let new_offset = if vma.mappable.upgrade().is_none() {
                0
            } else {
                self.mappable_range_of(vseg.range(), &vseg).end
            };
            match self.create_vma(&MmapOpts {
                length: new_size - old_size,
                mappable: vma.mappable.upgrade(),
                offset: new_offset,
                addr: old_end,
                fixed: true,
                perms: vma.real_perms,
                max_perms: vma.max_perms,
                private: vma.private,
                grows_down: vma.grows_down,
                mlock_mode: vma.mlock_mode,
                ..MmapOpts::default()
            }) {
                Ok((vseg, ar)) => {
                    if vma.mlock_mode == MLockMode::Eager {
                        self.populate_vma(&vseg, ar, true);
                    }
                    return Ok(old_addr);
                }
                Err(err) => {
                    if opts.mov == MremapMoveMode::No {
                        return Err(err);
                    }
                }
            };
        }

        let mut new_ar = AddrRange::default();
        match opts.mov {
            MremapMoveMode::May => {
                let new_addr = self.find_available(new_size, FindAvailableOpts::default())?;
                new_ar = new_addr.to_range(new_size).unwrap();
            }
            MremapMoveMode::Must => {
                let new_addr = opts.new_addr;
                if new_addr.round_down() != new_addr {
                    bail_libc!(libc::EINVAL);
                }
                new_ar = new_addr
                    .to_range(new_size)
                    .ok_or_else(|| SysError::new(libc::EINVAL))?;
                if (AddrRange {
                    start: old_addr.0,
                    end: old_end.0,
                })
                .overlaps(&new_ar)
                {
                    bail_libc!(libc::EINVAL);
                }
                self.find_available(
                    new_size,
                    FindAvailableOpts {
                        addr: new_addr,
                        fixed: true,
                        unmap: true,
                        map32bit: false,
                    },
                )?;
                self.unmap(new_ar);
                if new_size < old_size {
                    let old_new_end = old_addr.0 + new_size;
                    self.unmap(AddrRange {
                        start: old_new_end,
                        end: old_end.0,
                    });
                    old_end = Addr(old_new_end);
                }
                vseg = self.vmas.find_segment(old_addr.0).unwrap();
            }
            _ => (),
        };
        let old_ar = AddrRange {
            start: old_addr.0,
            end: old_end.0,
        };
        if vseg.end() < old_end.0 {
            bail_libc!(libc::EFAULT);
        }

        let new_usage_address_space = self.usage_address_space - old_ar.len() + new_ar.len();
        let ctx = context::context();
        let limit_as = ctx.limits().get_address_space().cur;
        if new_usage_address_space > limit_as {
            bail_libc!(libc::ENOMEM);
        }

        let vma = self.vmas.value(&vseg);
        if let Some(ref mappable) = vma.mappable.upgrade() {
            if vma.off + new_ar.len() < vma.off {
                bail_libc!(libc::EINVAL);
            }
            let off = self.mappable_offset_at(Addr(old_ar.start), &vseg);
            let writable = vma.can_write_mappable();
            mappable
                .borrow_mut()
                .copy_mapping(old_ar, new_ar, off, writable)?;
        }

        if old_size == 0 {
            let mut vma = self.vmas.value(&vseg).clone();
            if vma.mappable.upgrade().is_some() {
                vma.off = self.mappable_offset_at(Addr(old_ar.start), &vseg);
            }
            let is_private_data = vma.is_private_data();
            let lock_mode = vma.mlock_mode;
            let vseg = self.vmas.insert(new_ar, vma);
            self.usage_address_space += new_ar.len();
            if is_private_data {
                self.data_address_space += new_ar.len();
            }
            if lock_mode == MLockMode::None_ {
                self.locked_as += new_ar.len();
                if lock_mode != MLockMode::Eager {
                    self.populate_vma(&vseg, new_ar, true);
                }
            }
            return Ok(Addr(new_ar.start));
        }

        let vseg = self.vmas.isolate(&vseg, old_ar);
        let vma = self.vmas.value(&vseg).clone();
        self.vmas.remove(vseg.range());
        let is_private_data = vma.is_private_data();
        let mlock_mode = vma.mlock_mode;
        let vma_off = vma.off;
        let can_write_mappable = vma.can_write_mappable();
        let mappable = vma.mappable.clone();

        let vseg = self.vmas.insert(new_ar, vma);
        self.usage_address_space = self.usage_address_space - old_ar.len() + new_ar.len();
        if is_private_data {
            self.data_address_space = self.data_address_space - old_ar.len() + new_ar.len();
        }
        if mlock_mode != MLockMode::None_ {
            self.locked_as = self.locked_as - old_ar.len() + new_ar.len();
        }

        self.move_pmas(old_ar, new_ar);

        if let Some(ref mappable) = mappable.upgrade() {
            mappable
                .borrow_mut()
                .remove_mapping(old_ar, vma_off, can_write_mappable);
        }

        if mlock_mode == MLockMode::Eager {
            self.populate_vma(&vseg, new_ar, true);
        }

        Ok(Addr(new_ar.start))
    }

    pub fn mprotect(
        &mut self,
        addr: Addr,
        length: u64,
        real_perms: AccessType,
        grows_down: bool,
    ) -> SysResult<()> {
        if addr.round_down() != addr {
            bail_libc!(libc::EINVAL);
        }
        if length == 0 {
            return Ok(());
        }
        let rlength = Addr(length)
            .round_up()
            .ok_or_else(|| SysError::new(libc::ENOMEM))?;
        let mut ar = addr
            .to_range(rlength.0)
            .ok_or_else(|| SysError::new(libc::ENOMEM))?;
        let effective_perms = real_perms.effective();

        let mut vseg = self
            .vmas
            .lower_bound_segment(ar.start)
            .ok_or_else(|| SysError::new(libc::ENOMEM))?;
        if grows_down {
            if !self.vmas.value(&vseg).grows_down {
                bail_libc!(libc::EINVAL);
            }
            if ar.end <= vseg.start() {
                bail_libc!(libc::ENOMEM);
            }
            ar.start = vseg.start();
        } else if ar.start < vseg.start() {
            bail_libc!(libc::ENOMEM);
        }

        let mut pseg = self.pmas.lower_bound_segment(ar.start);
        let mut did_unmap_as = false;
        loop {
            if !self
                .vmas
                .value(&vseg)
                .max_perms
                .is_superset_of(effective_perms)
            {
                self.vmas.merge_range(ar);
                self.vmas.merge_adjacant(ar);
                self.pmas.merge_range(ar);
                self.pmas.merge_adjacant(ar);
                bail_libc!(libc::EACCES);
            }
            vseg = self.vmas.isolate(&vseg, ar);

            let vma = self.vmas.value_mut(&vseg);
            let vma_length = vseg.range().len();
            if vma.is_private_data() {
                self.data_address_space -= vma_length;
            }

            vma.real_perms = real_perms;
            vma.effective_perms = effective_perms;
            if vma.is_private_data() {
                self.data_address_space += vma_length;
            }

            while pseg.map_or(false, |s| s.start() < vseg.end()) {
                if pseg.unwrap().range().overlaps(&vseg.range()) {
                    pseg = Some(self.pmas.isolate(&pseg.unwrap(), vseg.range()));
                    if !effective_perms
                        .is_superset_of(self.pmas.value(&pseg.unwrap()).effective_perms)
                        && !did_unmap_as
                    {
                        self.unmap_address_space(ar);
                        did_unmap_as = true;
                    }
                    let pma = self.pmas.value_mut(&pseg.unwrap());
                    pma.effective_perms = effective_perms.intersect(pma.translate_perms);
                    if pma.need_cow {
                        pma.effective_perms.write = true;
                    }
                }
                pseg = self.pmas.next_segment_of_seg(&pseg.unwrap());
            }

            if ar.end <= vseg.end() {
                self.vmas.merge_range(ar);
                self.vmas.merge_adjacant(ar);
                self.pmas.merge_range(ar);
                self.pmas.merge_adjacant(ar);
                return Ok(());
            }
            match self.vmas.next_non_empty(&vseg) {
                Some(SegOrGap::Gap(_)) | None => {
                    self.vmas.merge_range(ar);
                    self.vmas.merge_adjacant(ar);
                    self.pmas.merge_range(ar);
                    self.pmas.merge_adjacant(ar);
                    bail_libc!(libc::ENOMEM);
                }
                Some(SegOrGap::Segment(s)) => vseg = s,
            };
        }
    }

    pub fn brk_setup(&mut self, addr: Addr) {
        if !self.brk.is_empty() {
            self.unmap(self.brk);
        }
        self.brk = AddrRange {
            start: addr.0,
            end: addr.0,
        };
    }

    // NOTE: "However, the actual Linux system call returns the new
    // program break on success.  On failure, the system call returns
    // the current break." - https://man7.org/linux/man-pages/man2/sbrk.2.html
    pub fn brk(&mut self, addr: Addr) -> Addr {
        if addr.0 < self.brk.start {
            return Addr(self.brk.end);
        }
        let ctx = context::context();
        if addr.0 - self.brk.start > ctx.limits().get_data().cur {
            return Addr(self.brk.end);
        }

        let old_brkpg = Addr(self.brk.end).round_up().unwrap();
        let new_brkpg = match addr.round_up() {
            Some(a) => a,
            None => return Addr(self.brk.end),
        };

        if old_brkpg.0 < new_brkpg.0 {
            let (vseg, ar) = match self.create_vma(&MmapOpts {
                length: (new_brkpg - old_brkpg).0,
                addr: old_brkpg,
                fixed: true,
                perms: AccessType::read_write(),
                max_perms: AccessType::any_access(),
                private: true,
                mlock_mode: self.def_mlock_mode,
                ..MmapOpts::default()
            }) {
                Ok(r) => r,
                Err(_) => return Addr(self.brk.end),
            };
            self.brk.end = addr.0;
            if self.def_mlock_mode == MLockMode::Eager {
                self.populate_vma(&vseg, ar, true);
            }
        } else {
            if new_brkpg < old_brkpg {
                self.unmap(AddrRange {
                    start: new_brkpg.0,
                    end: old_brkpg.0,
                });
            }
            self.brk.end = addr.0;
        }
        addr
    }

    fn create_vma(&mut self, opts: &MmapOpts) -> SysResult<(Seg<u64>, AddrRange)> {
        if opts.max_perms != opts.max_perms.effective() {
            panic!(
                "Non-effective max_perms {:?} cannot be enforced",
                opts.max_perms
            );
        }
        let addr = match self.find_available(
            opts.length,
            FindAvailableOpts {
                addr: opts.addr,
                fixed: opts.fixed,
                unmap: opts.unmap,
                map32bit: opts.map32bit,
            },
        ) {
            Ok(a) => a,
            Err(err) => {
                if opts.force && opts.unmap && opts.fixed {
                    opts.addr
                } else {
                    return Err(err);
                }
            }
        };
        let ar = addr.to_range(opts.length).expect("should handle?");
        let mut new_usage_address_space = self.usage_address_space + opts.length;
        if opts.unmap {
            new_usage_address_space -= self.vmas.span_range(ar);
        }
        let ctx = context::context();
        let limit_as = ctx.limits().get_address_space().cur;
        if new_usage_address_space > limit_as {
            bail_libc!(libc::ENOMEM);
        }

        if opts.mlock_mode != MLockMode::None_ {
            let creds = ctx.credentials();
            let root = UserNamespace::get_root(&creds.user_namespace);
            if !creds.has_capability_in(&linux::Capability::ipc_lock(), root) {
                let mlock_limit = ctx.limits().get_memory_locked().cur;
                if mlock_limit == 0 {
                    bail_libc!(libc::EPERM);
                }
                let mut new_locked_as = self.locked_as + opts.length;
                if opts.unmap {
                    new_locked_as -= self.mlocked_bytes_range(ar);
                }
                if new_locked_as > mlock_limit {
                    bail_libc!(libc::EAGAIN);
                }
            }
        }

        let vgap = if opts.unmap {
            self.unmap(ar)
        } else {
            self.vmas.find_gap(ar.start)
        }
        .unwrap();
        assert!(vgap.range().is_superset_of(&ar));

        if let Some(ref mappable) = opts.mappable {
            mappable.borrow_mut().add_mapping(
                ar,
                opts.offset,
                !opts.private && opts.max_perms.write,
            )?;
        }

        let mappable = match opts.mappable {
            Some(ref m) => Rc::downgrade(m),
            None => Weak::<RefCell<SpecialMappable>>::new(), // XXX: workaround to avoid compile error. better way?
        };

        let v = Vma {
            mappable,
            off: opts.offset,
            real_perms: opts.perms,
            effective_perms: opts.perms.effective(),
            max_perms: opts.max_perms,
            private: opts.private,
            grows_down: opts.grows_down,
            mlock_mode: opts.mlock_mode,
            numa_policy: linux::NumaPolicy::default(),
            numa_nodemask: 0,
        };

        let is_private_data = v.is_private_data();
        let vseg = self.vmas.insert(ar, v);
        self.usage_address_space += opts.length;
        if is_private_data {
            self.data_address_space += opts.length;
        }
        if opts.mlock_mode != MLockMode::None_ {
            self.locked_as += opts.length;
        }
        Ok((vseg, ar))
    }

    fn find_available(&self, length: u64, mut opts: FindAvailableOpts) -> SysResult<Addr> {
        if opts.fixed {
            opts.map32bit = false;
        }
        let mut allowed_ar = self.application_addr_range();
        if opts.map32bit {
            allowed_ar = allowed_ar.intersect(&AddrRange {
                start: MAP32START,
                end: MAP32END,
            });
        }

        if let Some(ar) = opts.addr.to_range(length) {
            if allowed_ar.is_superset_of(&ar) {
                if opts.unmap {
                    return Ok(Addr(ar.start));
                }
                let vgap = self.vmas.find_gap(ar.start as u64);
                if vgap.map_or(false, |g| self.available_range_of(&g).is_superset_of(&ar)) {
                    return Ok(Addr(ar.start));
                }
            }
        }

        if opts.fixed {
            bail_libc!(libc::ENOMEM);
        }

        let alignment = if length >= HUGE_PAGE_SIZE {
            HUGE_PAGE_SIZE as u64
        } else {
            PAGE_SIZE as u64
        };

        if opts.map32bit {
            self.find_lowest_available(length, alignment, allowed_ar)
        } else if self.layout.default_direction == MmapDirection::MmapBottomUp {
            self.find_lowest_available(
                length,
                alignment,
                AddrRange {
                    start: self.layout.bottom_up_base.0 as u64,
                    end: self.layout.max_addr.0 as u64,
                },
            )
        } else {
            self.find_highest_available(
                length,
                alignment,
                AddrRange {
                    start: self.layout.min_addr.0 as u64,
                    end: self.layout.top_down_base.0 as u64,
                },
            )
        }
    }

    fn find_lowest_available(
        &self,
        length: u64,
        alignment: u64,
        bounds: AddrRange,
    ) -> SysResult<Addr> {
        let mut maybe_gap = self.vmas.lower_bound_gap(bounds.start);
        while maybe_gap.map_or(false, |g| g.start() < bounds.end) {
            let gap = maybe_gap.unwrap();
            let gr = self.available_range_of(&gap).intersect(&bounds);
            if gr.len() >= length {
                let offset = gr.start % alignment;
                return if offset != 0 && gr.len() >= length + alignment - offset {
                    Ok(Addr(gr.start + alignment - offset))
                } else {
                    Ok(Addr(gr.start))
                };
            }
            maybe_gap = self.vmas.next_large_enough_gap(&gap, length);
        }
        bail_libc!(libc::ENOMEM);
    }

    fn find_highest_available(
        &self,
        length: u64,
        alignment: u64,
        bounds: AddrRange,
    ) -> SysResult<Addr> {
        let mut maybe_gap = self.vmas.upper_bound_gap(bounds.end);
        while maybe_gap.map_or(false, |g| g.end() > bounds.start) {
            let gap = maybe_gap.unwrap();
            let gr = self.available_range_of(&gap).intersect(&bounds);
            if gr.len() >= length {
                let start = gr.end - length;
                let offset = start % alignment;
                if offset != 0 && gr.start <= start - offset {
                    return Ok(Addr(start - offset));
                }
                return Ok(Addr(start));
            }
            maybe_gap = self.vmas.prev_large_enough_gap(&gap, length);
        }
        bail_libc!(libc::ENOMEM);
    }

    fn available_range_of(&self, vgap: &Gap<u64>) -> AddrRange {
        let mut ar = vgap.range();
        let next = self.vmas.next_segment_of_gap(vgap);
        if !next.map_or(false, |n| self.vmas.value(&n).grows_down) {
            ar
        } else if ar.len() < GUARD_BYTES as u64 {
            AddrRange {
                start: ar.start,
                end: ar.start,
            }
        } else {
            ar.end -= GUARD_BYTES as u64;
            ar
        }
    }

    fn mlocked_bytes_range(&self, ar: AddrRange) -> u64 {
        let mut total = 0;
        let mut vseg_maybe = self.vmas.lower_bound_segment(ar.start);
        while vseg_maybe.map_or(false, |v| v.start() < ar.end) {
            let vseg = vseg_maybe.unwrap();
            if self.vmas.value(&vseg).mlock_mode != MLockMode::None_ {
                total += vseg.range().intersect(&ar).len();
            }
            vseg_maybe = self.vmas.next_segment_of_seg(&vseg);
        }
        total
    }

    fn unmap(&mut self, ar: AddrRange) -> Option<Gap<u64>> {
        self.invalidate(
            ar,
            InvalidateOpts {
                invalidate_private: true,
            },
        );
        self.remove_vmas(ar)
    }

    fn remove_vmas(&mut self, ar: AddrRange) -> Option<Gap<u64>> {
        let mut vgap = self.vmas.find_gap(ar.start);
        let mut vseg = self.vmas.find_segment(ar.start);
        if let Some(vgap) = vgap {
            vseg = self.vmas.next_segment_of_gap(&vgap);
        }

        while vseg.map_or(false, |s| s.start() < ar.end) {
            let vseg_inner = self.vmas.isolate(&vseg.unwrap(), ar);
            let vma_ar = vseg_inner.range();
            let vma = self.vmas.value(&vseg_inner);
            if let Some(ref mappable) = vma.mappable.upgrade() {
                mappable
                    .borrow_mut()
                    .remove_mapping(vma_ar, vma.off, vma.can_write_mappable());
            }
            let vma_ar_length = vma_ar.len();
            self.usage_address_space -= vma_ar_length;
            if vma.is_private_data() {
                self.data_address_space -= vma_ar_length;
            }
            if vma.mlock_mode != MLockMode::None_ {
                self.locked_as -= vma_ar_length;
            }
            vgap = Some(self.vmas.remove(vseg_inner.range()));
            vseg = self.vmas.next_segment_of_gap(&vgap.unwrap());
        }
        vgap
    }

    fn populate_vma(&mut self, vseg: &Seg<u64>, ar: AddrRange, precommit: bool) {
        if !self.vmas.value(vseg).effective_perms.any() {
            return;
        }
        if self.address_space.is_none() {
            return;
        }
        let (pseg, _, res) = self.get_pmas(*vseg, ar, AccessType::no_access());
        if res.is_err() {
            return;
        }
        self.map_address_space(pseg.unwrap(), ar, precommit)
            .unwrap();
    }

    fn map_address_space(
        &mut self,
        mut pseg: Seg<u64>,
        ar: AddrRange,
        precommit: bool,
    ) -> SysResult<()> {
        let ctx = &*context::context();
        let map_unit = {
            let platform = ctx.platform();
            platform.map_unit()
        };
        let map_ar = if precommit {
            ar
        } else if map_unit != 0 {
            let map_mask = map_unit - 1;
            let end = (ar.end + map_mask) & !map_mask;
            let end = if end >= ar.end {
                end
            } else {
                !(PAGE_SIZE as u64 - 1)
            };
            AddrRange {
                start: ar.start & !map_mask,
                end,
            }
        } else {
            AddrRange {
                start: 0,
                end: !(PAGE_SIZE as u64 - 1),
            }
        };

        while pseg.start() < ar.end {
            let pma = self.pmas.value(&pseg);
            let pma_ar = pseg.range();
            let pma_map_ar = pma_ar.intersect(&map_ar);
            let mut perms = pma.effective_perms;
            if pma.need_cow {
                perms.write = false;
            }
            if perms.any() {
                let pma_file = pma.file.upgrade().unwrap();
                let pma_file = pma_file.read().unwrap();
                let (pma_file_fd, should_close) = pma_file.fd();
                self.address_space.as_ref().unwrap().map_file(
                    Addr(pma_map_ar.start),
                    pma_file_fd,
                    self.file_range_of(&pseg, pma_map_ar),
                    perms,
                    precommit,
                    ctx,
                )?;
                if should_close {
                    pma_file.close()
                }
            }
            pseg = match self.pmas.next_segment_of_seg(&pseg) {
                Some(s) => s,
                None => break,
            };
        }
        Ok(())
    }

    fn move_pmas(&mut self, old_ar: AddrRange, new_ar: AddrRange) {
        debug_assert!(old_ar.is_well_formed());
        debug_assert!(!old_ar.is_empty());
        debug_assert!(new_ar.is_well_formed());
        debug_assert!(!new_ar.is_empty());
        debug_assert!(old_ar.len() <= new_ar.len());
        debug_assert!(!old_ar.overlaps(&new_ar));

        let mut moved_pmas = Vec::new();
        let mut pseg = self.pmas.lower_bound_segment(old_ar.start);
        while pseg.map_or(false, |s| s.start() < old_ar.end) {
            let pseg_inner = self.pmas.isolate(&pseg.unwrap(), old_ar);
            pseg = Some(pseg_inner);
            moved_pmas.push(MovedPma {
                old_ar: pseg.unwrap().range(),
                pma: self.pmas.value(&pseg_inner).clone(),
            });
            let removed = self.pmas.remove(pseg_inner.range());
            pseg = self.pmas.next_segment_of_gap(&removed);
        }
        let off = if new_ar.start >= old_ar.start {
            (new_ar.start - old_ar.start) as i64
        } else {
            -((old_ar.start - new_ar.start) as i64)
        };
        for mpma in &moved_pmas {
            let (start, end) = if off >= 0 {
                (
                    mpma.old_ar.start + (off as u64),
                    mpma.old_ar.end + (off as u64),
                )
            } else {
                (
                    mpma.old_ar.start - (-off as u64),
                    mpma.old_ar.end - (-off as u64),
                )
            };
            let pma_new_ar = AddrRange { start, end };
            self.pmas.insert(pma_new_ar, mpma.pma.clone());
        }
        self.unmap_address_space(old_ar);
    }

    pub fn map_stack(&mut self) -> SysResult<AddrRange> {
        const MAX_STACK_SIZE: u64 = 128 << 20;
        let stack_size = {
            let ctx = context::context();
            ctx.limits().get_stack()
        };
        let sz = match Addr(stack_size.cur).round_up() {
            Some(sz) => {
                if sz == Addr(0) {
                    bail_libc!(libc::ENOMEM);
                } else {
                    Addr(std::cmp::min(sz.0, MAX_STACK_SIZE))
                }
            }
            None => Addr(linux::DEFAULT_STACK_SOFT_LIMIT),
        };

        let stack_end = {
            let mut rng = rand::thread_rng();
            self.layout.max_addr - Addr(rng.gen_range(0..self.layout.max_stack_rand)).round_down()
        };
        if stack_end < sz {
            bail_libc!(libc::ENOMEM);
        }
        let stack_start = stack_end - sz;
        let ret = self.create_vma(&MmapOpts {
            length: sz.0,
            addr: stack_start,
            perms: AccessType::read_write(),
            max_perms: AccessType::any_access(),
            private: true,
            grows_down: true,
            ..MmapOpts::default()
        })?;
        Ok(ret.1)
    }

    #[cfg(test)]
    fn real_usage_address_space(&self) -> u64 {
        self.vmas.span()
    }

    #[cfg(test)]
    fn real_data_address_space(&self) -> u64 {
        let mut sz = 0;
        let mut maybe_seg = self.vmas.first_segment();
        while let Some(seg) = maybe_seg {
            let vma = self.vmas.value(&seg);
            if vma.is_private_data() {
                sz += seg.range().len();
            }
            maybe_seg = self.vmas.next_segment_of_seg(&seg);
        }
        sz
    }

    pub fn print_vmas(&self) {
        logger::debug!("printing vma keys");
        self.vmas.print_keys();
        logger::debug!("done");
    }
}

impl mem::io::Io for MemoryManager {
    fn copy_out(&mut self, addr: Addr, src: &[u8], opts: &IoOpts) -> SysResult<usize> {
        let ar = self
            .check_io_range(addr, src.len() as i64)
            .ok_or_else(|| SysError::new(libc::EFAULT))?;
        if src.is_empty() {
            return Ok(0);
        }

        self.with_internal_mappings(ar, AccessType::write(), opts.ignore_permissions, |ims| {
            let b = &[Block::from_slice(src, false)];
            copy_seq(ims, BlockSeqView::from_slice(b))
        })
    }

    fn copy_in(&mut self, addr: Addr, dst: &mut [u8], opts: &IoOpts) -> SysResult<usize> {
        let ar = self
            .check_io_range(addr, dst.len() as i64)
            .ok_or_else(|| SysError::new(libc::EFAULT))?;

        if dst.is_empty() {
            return Ok(0);
        }

        self.with_internal_mappings(ar, AccessType::read(), opts.ignore_permissions, |ims| {
            let b = &[Block::from_slice(dst, false)];
            copy_seq(BlockSeqView::from_slice(b), ims)
        })
    }

    fn zero_out(&mut self, addr: Addr, to_zero: i64, opts: &IoOpts) -> SysResult<usize> {
        let ar = self
            .check_io_range(addr, to_zero)
            .ok_or_else(|| SysError::new(libc::EFAULT))?;
        if to_zero == 0 {
            return Ok(0);
        }
        self.with_internal_mappings(ar, AccessType::write(), opts.ignore_permissions, zero_seq)
    }

    fn copy_out_from(
        &mut self,
        ars: AddrRangeSeqView,
        src: &mut dyn mem::io::Reader,
        opts: &IoOpts,
    ) -> SysResult<usize> {
        if !self.check_io_vec(ars) {
            bail_libc!(libc::EFAULT);
        }
        if ars.num_bytes() == 0 {
            return Ok(0);
        }
        self.with_vec_internal_mappings(ars, AccessType::write(), opts.ignore_permissions, |dsts| {
            src.read_to_blocks(dsts)
        })
    }

    fn copy_in_to(
        &mut self,
        ars: AddrRangeSeqView,
        dst: &mut dyn mem::io::Writer,
        opts: &IoOpts,
    ) -> SysResult<usize> {
        if !self.check_io_vec(ars) {
            bail_libc!(libc::EFAULT);
        }
        if ars.num_bytes() == 0 {
            return Ok(0);
        }
        self.with_vec_internal_mappings(ars, AccessType::read(), opts.ignore_permissions, |src| {
            dst.write_from_blocks(src)
        })
    }
}

impl memmap::MemoryInvalidator for MemoryManager {
    fn invalidate(&mut self, ar: AddrRange, opts: InvalidateOpts) {
        if self.capture_invalidations {
        } else {
            let mut did_unmap_as = false;
            let mut pseg_maybe = self.pmas.lower_bound_segment(ar.start);
            while pseg_maybe.map_or(false, |s| s.start() < ar.end) {
                let pseg = pseg_maybe.unwrap();
                let pma = self.pmas.value(&pseg);
                if opts.invalidate_private || !pma.private {
                    let pseg = self.pmas.isolate(&pseg, ar);
                    if !did_unmap_as {
                        self.unmap_address_space(ar);
                        did_unmap_as = true;
                    }
                    self.remove_rss(pseg.range());
                    let removed = self.pmas.remove(pseg.range());
                    pseg_maybe = self.pmas.next_segment_of_gap(&removed);
                } else {
                    pseg_maybe = self.pmas.next_segment_of_seg(&pseg);
                }
            }
        }
    }
}

struct MovedPma {
    old_ar: AddrRange,
    pma: Pma,
}

#[derive(Default)]
struct FindAvailableOpts {
    addr: Addr,
    fixed: bool,
    unmap: bool,
    map32bit: bool,
}

fn truncated_addr_range_seq<'a>(
    ars: AddrRangeSeqView<'a>,
    arsit: AddrRangeSeqView<'a>,
    end: u64,
) -> AddrRangeSeqView<'a> {
    let ar = arsit.head();
    if end <= ar.start {
        ars.take_first(ars.num_bytes() - arsit.num_bytes())
    } else {
        ars.take_first(ars.num_bytes() - arsit.num_bytes() + (end - ar.start) as usize)
    }
}

fn translate_io_error(err: SysError) -> SysError {
    logger::warn!("MM I/O error: {:?}", err);
    SysError::new(libc::EFAULT)
}

const PRIVATE_ALLOC_UNIT: u64 = HUGE_PAGE_SIZE;
const PRIVATE_ALLOC_MASK: u64 = PRIVATE_ALLOC_UNIT - 1;

fn private_aligned(ar: AddrRange) -> AddrRange {
    let mut aligned = AddrRange {
        start: ar.start & !PRIVATE_ALLOC_MASK,
        end: ar.end,
    };
    let end = (ar.end + PRIVATE_ALLOC_MASK) & !PRIVATE_ALLOC_MASK;
    if end >= ar.end {
        aligned.end = end;
    }
    aligned
}

type FileRefcountSet = Set<u64, i32>;
struct FileRefcountSetOperations;
impl SetOperations for FileRefcountSetOperations {
    type K = u64;
    type V = i32;
    fn merge(
        &self,
        _r1: Range<Self::K>,
        v1: &Self::V,
        _r2: Range<Self::K>,
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

#[derive(Debug)]
struct PrivateRefs {
    refs: FileRefcountSet,
}

impl PrivateRefs {
    fn new() -> Self {
        let ops = FileRefcountSetOperations;
        Self {
            refs: FileRefcountSet::new(Box::new(ops)),
        }
    }
}

#[derive(Debug)]
pub struct SpecialMappable {
    file_range: FileRange,
    _name: String,
}

impl SpecialMappable {
    pub fn new(_name: String, file_range: FileRange) -> SpecialMappable {
        SpecialMappable { file_range, _name }
    }

    pub fn new_anon(length: u64) -> SysResult<Self> {
        if length == 0 {
            bail_libc!(libc::EINVAL);
        }
        let aligned_len = Addr(length)
            .round_up()
            .ok_or_else(|| SysError::new(libc::EINVAL))?;
        let fr = {
            let ctx = context::context();
            let mut mf = ctx.kernel().memory_file_write_lock();
            mf.allocate(
                aligned_len.0,
                AllocOpts {
                    kind: MemoryKind::Anonymous,
                    dir: Direction::BottomUp,
                },
            )?
        };
        Ok(Self::new(String::from("/dev/zero (deleted)"), fr))
    }

    pub fn len(&self) -> u64 {
        self.file_range.len()
    }
}

impl Mappable for SpecialMappable {
    fn translate(
        &self,
        required: MappableRange,
        optional: MappableRange,
        _: AccessType,
    ) -> (Vec<Translation>, SysResult<()>) {
        let res = if required.end > self.file_range.len() {
            Err(SysError::new_bus_error(libc::EFAULT))
        } else {
            Ok(())
        };
        let source = optional.intersect(&MappableRange {
            start: 0,
            end: self.file_range.len(),
        });
        if !source.is_empty() {
            let ctx = context::context();
            (
                vec![Translation::new(
                    source,
                    Rc::<RwLock<MemoryFile>>::downgrade(ctx.kernel().memory_file()),
                    self.file_range.start + source.start,
                    AccessType::any_access(),
                )],
                res,
            )
        } else {
            (vec![], res)
        }
    }
    fn add_mapping(&mut self, _: AddrRange, _: u64, _: bool) -> SysResult<()> {
        Ok(())
    }
    fn remove_mapping(&mut self, _: AddrRange, _: u64, _: bool) {}
    fn copy_mapping(&mut self, _: AddrRange, _: AddrRange, _: u64, _: bool) -> SysResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use limit::{Limit, LimitSet};
    use mem::io::Io;

    fn memory_manager() -> Rc<RefCell<MemoryManager>> {
        context::init_for_test();
        let platform = {
            let ctx = context::context();
            ctx.platform()
        };
        let mut mm = MemoryManager::new();
        mm.layout = MmapLayout::new_test(
            platform.min_user_address(),
            platform.max_user_address(),
            platform.min_user_address(),
            platform.max_user_address(),
        );
        Rc::new(RefCell::new(mm))
    }

    #[test]
    fn usage_address_space_updates() {
        let mm = memory_manager();
        let mut mm = mm.as_ref().borrow_mut();

        let addr = mm
            .mmap(MmapOpts {
                length: 2 * PAGE_SIZE as u64,
                private: true,
                ..MmapOpts::default()
            })
            .expect("error occurred in mmap");

        assert_eq!(mm.usage_address_space, mm.real_usage_address_space());
        mm.munmap(addr, PAGE_SIZE as u64)
            .expect("error occurred in munmap");
        assert_eq!(mm.usage_address_space, mm.real_usage_address_space());
    }

    #[test]
    fn data_address_space_updates() {
        let mm = memory_manager();
        let mut mm = mm.as_ref().borrow_mut();

        let addr = mm
            .mmap(MmapOpts {
                length: 3 * PAGE_SIZE as u64,
                private: true,
                perms: AccessType::write(),
                max_perms: AccessType::any_access(),
                ..MmapOpts::default()
            })
            .expect("error occurred in mmap");

        assert_ne!(mm.data_address_space, 0);
        assert_eq!(mm.data_address_space, mm.real_data_address_space());

        mm.munmap(addr, PAGE_SIZE as u64)
            .expect("error occurred in munmap");
        assert_eq!(mm.data_address_space, mm.real_data_address_space());

        mm.mprotect(
            Addr(addr.0 + PAGE_SIZE as u64),
            PAGE_SIZE as u64,
            AccessType::read(),
            false,
        )
        .expect("error occurred in mprotect");
        assert_eq!(mm.data_address_space, mm.real_data_address_space());

        mm.mremap(
            Addr(addr.0 + 2 * PAGE_SIZE as u64),
            PAGE_SIZE as u64,
            2 * PAGE_SIZE as u64,
            &MremapOpts {
                mov: MremapMoveMode::May,
                new_addr: Addr(0),
            },
        )
        .expect("error occurred in mremap");
        assert_eq!(mm.data_address_space, mm.real_data_address_space());
    }

    #[test]
    fn brk_data_limit_updates() {
        let mm = memory_manager();

        let mut limit_set = LimitSet::default();
        limit_set.set_data(Limit::default(), true).unwrap();

        {
            let mut ctx = context::context_mut();
            ctx.set_limits(limit_set);
        }

        let mut mm = mm.as_ref().borrow_mut();
        let old_brk = mm.brk(Addr(0));
        let new_brk = mm.brk(Addr(old_brk.0 + PAGE_SIZE as u64));
        assert_eq!(old_brk, new_brk);
    }

    #[test]
    fn io_after_unmap() {
        let mm = memory_manager();

        let mut mm = mm.as_ref().borrow_mut();
        let addr = mm
            .mmap(MmapOpts {
                length: PAGE_SIZE as u64,
                private: true,
                perms: AccessType::read(),
                max_perms: AccessType::any_access(),
                ..MmapOpts::default()
            })
            .expect("error occurred in mmap");

        let mut b = vec![0];
        let n = mm.copy_in(addr, &mut b, &IoOpts::default());
        assert_eq!(n, Ok(1));

        mm.munmap(addr, PAGE_SIZE as u64)
            .expect("error occurred in munmap");

        let res = mm.copy_in(addr, &mut b, &IoOpts::default());
        assert_eq!(res, Err(SysError::new(libc::EFAULT)));
    }

    #[test]
    fn io_after_mprotect() {
        let mm = memory_manager();

        let mut mm = mm.as_ref().borrow_mut();
        let addr = mm
            .mmap(MmapOpts {
                length: PAGE_SIZE as u64,
                private: true,
                perms: AccessType::read_write(),
                max_perms: AccessType::any_access(),
                ..MmapOpts::default()
            })
            .expect("error occurred in mmap");

        let b = vec![0];
        let n = mm.copy_out(addr, &b, &IoOpts::default());
        assert_eq!(n, Ok(1));

        mm.mprotect(addr, PAGE_SIZE as u64, AccessType::read(), false)
            .expect("error occurred in mprotect");

        let res = mm.copy_out(addr, &b, &IoOpts::default());
        assert_eq!(res, Err(SysError::new(libc::EFAULT)));

        let n = mm.copy_out(
            addr,
            &b,
            &IoOpts {
                ignore_permissions: true,
            },
        );
        assert_eq!(n, Ok(1));
    }
}
