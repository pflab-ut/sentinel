use std::{
    fs::File as StdFile,
    os::unix::prelude::FromRawFd,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

use once_cell::sync::OnceCell;

use utils::mem::create_mem_fd;

use super::MemoryKind;

#[derive(Debug)]
#[repr(C)]
pub struct RTMemoryStats {
    rt_mapped: u64,
}

#[derive(Debug)]
pub struct MemoryLocked {
    system: AtomicU64,
    anonymous: AtomicU64,
    page_cache: AtomicU64,
    tmpfs: AtomicU64,
    mapped: AtomicU64,
    ramdiskfs: AtomicU64,
    rt_mapped: AtomicU64,
    file: Rc<StdFile>,
    rt_memory_stats: Rc<RTMemoryStats>,
}

impl Clone for MemoryLocked {
    fn clone(&self) -> Self {
        Self {
            system: AtomicU64::new(self.system.load(Ordering::SeqCst)),
            anonymous: AtomicU64::new(self.anonymous.load(Ordering::SeqCst)),
            page_cache: AtomicU64::new(self.page_cache.load(Ordering::SeqCst)),
            tmpfs: AtomicU64::new(self.tmpfs.load(Ordering::SeqCst)),
            mapped: AtomicU64::new(self.rt_mapped.load(Ordering::SeqCst)), // not mapped
            ramdiskfs: AtomicU64::new(self.ramdiskfs.load(Ordering::SeqCst)),
            rt_mapped: AtomicU64::new(self.rt_mapped.load(Ordering::SeqCst)),
            file: Rc::clone(&self.file),
            rt_memory_stats: Rc::clone(&self.rt_memory_stats),
        }
    }
}

unsafe impl Send for MemoryLocked {}
unsafe impl Sync for MemoryLocked {}

impl MemoryLocked {
    pub fn mapped(&self) -> u64 {
        self.mapped.load(Ordering::SeqCst)
    }

    fn inc(&self, val: u64, kind: MemoryKind) {
        match kind {
            MemoryKind::System => self.system.fetch_add(val, Ordering::SeqCst),
            MemoryKind::Anonymous => self.anonymous.fetch_add(val, Ordering::SeqCst),
            MemoryKind::PageCache => self.page_cache.fetch_add(val, Ordering::SeqCst),
            MemoryKind::Mapped => self.rt_mapped.fetch_add(val, Ordering::SeqCst),
            MemoryKind::Tmpfs => self.tmpfs.fetch_add(val, Ordering::SeqCst),
            MemoryKind::Ramdiskfs => self.ramdiskfs.fetch_add(val, Ordering::SeqCst),
        };
    }

    fn dec(&self, val: u64, kind: MemoryKind) {
        match kind {
            MemoryKind::System => self.system.fetch_add(!(val - 1), Ordering::SeqCst),
            MemoryKind::Anonymous => self.anonymous.fetch_add(!(val - 1), Ordering::SeqCst),
            MemoryKind::PageCache => self.page_cache.fetch_add(!(val - 1), Ordering::SeqCst),
            MemoryKind::Mapped => self.rt_mapped.fetch_add(!(val - 1), Ordering::SeqCst),
            MemoryKind::Tmpfs => self.tmpfs.fetch_add(!(val - 1), Ordering::SeqCst),
            MemoryKind::Ramdiskfs => self.ramdiskfs.fetch_add(!(val - 1), Ordering::SeqCst),
        };
    }

    pub fn change_memory_kind(&self, val: u64, to: MemoryKind, from: MemoryKind) {
        self.dec(val, from);
        self.inc(val, to);
    }

    pub fn total(&self) -> u64 {
        let mut total = self.system.load(Ordering::SeqCst);
        total += self.anonymous.load(Ordering::SeqCst);
        total += self.page_cache.load(Ordering::SeqCst);
        total += self.rt_mapped.load(Ordering::SeqCst);
        total += self.tmpfs.load(Ordering::SeqCst);
        total += self.ramdiskfs.load(Ordering::SeqCst);
        total
    }
}

pub static MEMORY_ACCOUNTING: OnceCell<MemoryLocked> = OnceCell::new();

pub fn init_memory_accounting() {
    static RT_MEMORY_STATS_SIZE: usize = std::mem::size_of::<RTMemoryStats>();
    let name = "memory-usage";
    let fd = match create_mem_fd(name, 0) {
        Ok(fd) => fd,
        Err(err) => panic!("error creating usage file: {:?}", err),
    };
    let file = unsafe { StdFile::from_raw_fd(fd) };
    if let Err(err) = file.set_len(RT_MEMORY_STATS_SIZE as u64) {
        panic!("error truncating usage file: {:?}", err);
    }
    let mmap = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            RT_MEMORY_STATS_SIZE,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    if mmap == libc::MAP_FAILED {
        panic!("error mapping usage file");
    }
    let rt_memory_stats = Rc::new(unsafe { std::ptr::read(mmap as *const RTMemoryStats) });
    let memory_accounting = MemoryLocked {
        system: AtomicU64::new(0),
        anonymous: AtomicU64::new(0),
        page_cache: AtomicU64::new(0),
        tmpfs: AtomicU64::new(0),
        mapped: AtomicU64::new(0),
        ramdiskfs: AtomicU64::new(0),
        rt_mapped: AtomicU64::new(0),
        file: Rc::new(file),
        rt_memory_stats,
    };
    if MEMORY_ACCOUNTING.get().is_none() {
        MEMORY_ACCOUNTING.set(memory_accounting).unwrap();
    }
}

const MINIMUM_TOTAL_MEMORY_BYTES: u64 = 2 << 30;
static MAXIMUM_TOTAL_MEMORY_BYTES: u64 = 0;

pub fn total_usable_memory(mem_size: u64, used: u64) -> u64 {
    let mut mem_size = std::cmp::max(mem_size, MINIMUM_TOTAL_MEMORY_BYTES);
    if mem_size < used {
        mem_size = used;
        let msb = utils::bit::msb(mem_size);
        if msb < 63 {
            mem_size = 1u64 << (msb + 1);
        }
    }
    if MAXIMUM_TOTAL_MEMORY_BYTES > 0 && mem_size > MAXIMUM_TOTAL_MEMORY_BYTES {
        MAXIMUM_TOTAL_MEMORY_BYTES
    } else {
        mem_size
    }
}
