pub mod epoll;
pub mod eventfd;
pub mod fd_table;
pub mod pipe;
pub mod task;
mod task_image;
mod uts_namespace;

use memmap::file::MemmapFile;
pub use uts_namespace::*;

use std::fs::File as StdFile;
use std::rc::Rc;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::{cell::RefCell, os::unix::prelude::FromRawFd};

use goblin::elf::program_header::ProgramHeader;
use mem::{
    block::Block,
    block_seq::{copy_seq, BlockSeq},
    AccessType, Addr, PAGE_SIZE,
};
use pgalloc::{AllocOpts, Direction, MemoryFile, MemoryFileOpts, MemoryFileProvider};
use platform::Platform;
use usage::MemoryKind;
use utils::mem::create_mem_fd;

use crate::mm::SpecialMappable;

#[derive(Debug)]
pub struct Vdso {
    pub param_page: Rc<RefCell<SpecialMappable>>,
    pub vdso: Rc<RefCell<SpecialMappable>>,
    pub phdrs: Vec<ProgramHeader>,
}

impl Vdso {
    pub fn prepare(mf: &mut MemoryFile) -> anyhow::Result<Self> {
        let bytes: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/vdso.so"));
        let elf = goblin::elf::Elf::parse(bytes)?;
        let size = Addr(bytes.len() as u64)
            .round_up()
            .ok_or_else(|| anyhow::anyhow!("VDSO size overflows? {}", bytes.len()))?;
        let vdso = mf
            .allocate(
                size.0 as u64,
                AllocOpts {
                    kind: MemoryKind::System,
                    dir: Direction::BottomUp,
                },
            )
            .map_err(|e| anyhow::anyhow!("unable to allocate VDSO memory: {:?}", e))?;
        let ims = mf.map_internal(vdso, AccessType::read_write())?;
        let src = BlockSeq::from_block(Block::from_slice(bytes, false));
        copy_seq(ims.as_view(), src.as_view())?;
        let param_page = mf.allocate(
            PAGE_SIZE as u64,
            AllocOpts {
                kind: MemoryKind::System,
                dir: Direction::BottomUp,
            },
        )?;
        Ok(Vdso {
            param_page: Rc::new(RefCell::new(SpecialMappable::new(
                "[vvar]".to_string(),
                param_page,
            ))),
            vdso: Rc::new(RefCell::new(SpecialMappable::new(String::new(), vdso))),
            phdrs: elf.program_headers,
        })
    }
}

#[derive(Debug)]
pub struct Kernel {
    platform: Platform,
    memory_file: Rc<RwLock<MemoryFile>>,
    vdso: Vdso,
    version: KernelVersion,
}

impl MemoryFileProvider for Kernel {
    fn memory_file(&self) -> &Rc<RwLock<MemoryFile>> {
        &self.memory_file
    }
    fn memory_file_read_lock(&self) -> RwLockReadGuard<'_, MemoryFile> {
        self.memory_file.read().unwrap()
    }
    fn memory_file_write_lock(&self) -> RwLockWriteGuard<'_, MemoryFile> {
        self.memory_file.write().unwrap()
    }
}

impl Kernel {
    pub fn platform(&self) -> Platform {
        self.platform
    }

    pub fn vdso(&self) -> &Vdso {
        &self.vdso
    }

    pub fn version(&self) -> &KernelVersion {
        &self.version
    }

    pub fn load() -> Self {
        let memfile_name = "sentinel-context-memory";
        let memfd = create_mem_fd(memfile_name, 0)
            .unwrap_or_else(|e| panic!("error creating application memory file: {:?}", e));
        let memfile = unsafe { StdFile::from_raw_fd(memfd) };
        let mut memory_file = MemoryFile::new(memfile, MemoryFileOpts::default())
            .expect("error creating pgalloc::MemoryFile");
        let vdso = Vdso::prepare(&mut memory_file).expect("failed to load vdso");

        Self {
            platform: Platform::Ptrace,
            memory_file: Rc::new(RwLock::new(memory_file)),
            vdso,
            version: KernelVersion::init(),
        }
    }
}

#[derive(Debug)]
pub struct KernelVersion {
    pub sysname: String,
    pub release: String,
    pub version: String,
}

impl KernelVersion {
    fn init() -> Self {
        // These strings are just copied from gVisor.
        Self {
            sysname: "Linux".to_string(),
            release: "4.4.0".to_string(),
            version: "#1 SMP Sun Jan 10 15:06:54 PST 2016".to_string(),
        }
    }
}
