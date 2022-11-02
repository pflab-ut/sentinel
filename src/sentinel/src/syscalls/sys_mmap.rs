use std::{cell::RefCell, rc::Rc};

use mem::{AccessType, Addr};
use memmap::mmap_opts::{MLockMode, MmapOpts};
use utils::{bail_libc, SysError};

use crate::{
    context,
    mm::{MremapMoveMode, MremapOpts, SpecialMappable},
};

// mmap implements linux syscall mmap(2)
pub fn mmap(regs: &libc::user_regs_struct) -> super::Result {
    let addr = Addr(regs.rdi);
    let length = regs.rsi;
    let prot = regs.rdx as i32;
    let flags = regs.r10 as i32;
    let fd = regs.r8 as i32;
    let offset = regs.r9;

    let fixed = flags & libc::MAP_FIXED != 0;
    let private = flags & libc::MAP_PRIVATE != 0;
    let shared = flags & libc::MAP_SHARED != 0;
    let anon = flags & libc::MAP_ANONYMOUS != 0;
    let map32bit = flags & libc::MAP_32BIT != 0;
    let mlock_mode = if flags & libc::MAP_LOCKED != 0 {
        MLockMode::Eager
    } else {
        MLockMode::default()
    };

    if private == shared {
        bail_libc!(libc::EINVAL);
    }

    let mut opts = MmapOpts {
        length,
        offset,
        addr,
        private,
        fixed,
        unmap: fixed,
        map32bit,
        grows_down: flags & libc::MAP_GROWSDOWN != 0,
        precommit: flags & libc::MAP_POPULATE != 0,
        perms: AccessType::from_prot(prot),
        max_perms: AccessType::any_access(),
        mlock_mode,
        ..MmapOpts::default()
    };

    let ctx = &*context::context();
    let mm = ctx.memory_manager();
    if !anon {
        let mut task = ctx.task_mut();
        let file = task
            .get_file(fd)
            .ok_or_else(|| SysError::new(libc::EBADF))?;
        let mut file = file.borrow_mut();
        let flags = file.flags();
        if !flags.read {
            bail_libc!(libc::EACCES);
        }
        if shared && !flags.write {
            opts.max_perms.write = false;
        }
        file.configure_mmap(&mut opts)?;
    } else if shared {
        opts.offset = 0;
        let m = SpecialMappable::new_anon(opts.length)?;
        let m = Rc::new(RefCell::new(m));
        opts.mappable = Some(m);
    }
    let mut mm = mm.borrow_mut();
    mm.mmap(opts).map(|n| n.0 as usize)
}

// munmap implements linux syscall munmap(2)
pub fn munmap(regs: &libc::user_regs_struct) -> super::Result {
    let mm = {
        let ctx = context::context();
        ctx.memory_manager()
    };
    let mut mm = mm.borrow_mut();
    mm.munmap(Addr(regs.rdi), regs.rsi).map(|()| 0)
}

// brk implements linux syscall brk(2)
pub fn brk(regs: &libc::user_regs_struct) -> super::Result {
    let mm = {
        let ctx = context::context();
        ctx.memory_manager()
    };
    let addr = regs.rdi;
    let addr = mm.borrow_mut().brk(Addr(addr));
    Ok(addr.0 as usize)
}

// mprotect implements linux syscall mprotect(2)
pub fn mprotect(regs: &libc::user_regs_struct) -> super::Result {
    let length = regs.rsi;
    let prot = regs.rdx as i32;
    let at = AccessType {
        read: libc::PROT_READ & prot != 0,
        write: libc::PROT_WRITE & prot != 0,
        execute: libc::PROT_EXEC & prot != 0,
    };
    let mm = {
        let ctx = context::context();
        ctx.memory_manager()
    };
    let mut mm = mm.borrow_mut();
    mm.mprotect(Addr(regs.rdi), length, at, libc::PROT_GROWSDOWN & prot != 0)
        .map(|()| 0)
}

// mremap implements linux syscall mremap(2)
pub fn mremap(regs: &libc::user_regs_struct) -> super::Result {
    let old_addr = Addr(regs.rdi);
    let old_size = regs.rsi;
    let new_size = regs.rdx;
    let flags = regs.r10 as i32;
    let new_addr = Addr(regs.r8);

    if flags & !(libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED) != 0 {
        bail_libc!(libc::EINVAL);
    }
    let may_move = flags & libc::MREMAP_MAYMOVE != 0;
    let fixed = flags & libc::MREMAP_FIXED != 0;
    let move_mode = if !may_move && !fixed {
        MremapMoveMode::No
    } else if may_move && !fixed {
        MremapMoveMode::May
    } else if !may_move && fixed {
        bail_libc!(libc::EINVAL);
    } else if may_move && fixed {
        MremapMoveMode::Must
    } else {
        unreachable!()
    };
    let mm = {
        let ctx = context::context();
        ctx.memory_manager()
    };
    let mut mm = mm.borrow_mut();
    mm.mremap(
        old_addr,
        old_size,
        new_size,
        &MremapOpts {
            mov: move_mode,
            new_addr,
        },
    )
    .map(|n| n.0 as usize)
}
