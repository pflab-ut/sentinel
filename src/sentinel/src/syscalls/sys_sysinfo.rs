use mem::Addr;
use pgalloc::Context as PgallocContext;
use time::Context as TimeContext;
use usage::memory::{total_usable_memory, MEMORY_ACCOUNTING};
use utils::SysError;

use crate::context;

// sysinfo implements linux syscall sysinfo(2)
pub fn sysinfo(regs: &libc::user_regs_struct) -> super::Result {
    let addr = Addr(regs.rdi);

    let ctx = context::context();
    let mf = ctx.memory_file_provider().memory_file();
    let mf = mf.read().unwrap();
    let mf_usage = mf.total_usage().map_err(SysError::from_nix_errno)?;
    let mem_stats = MEMORY_ACCOUNTING.get().unwrap().clone();
    let total_usage = mf_usage + mem_stats.mapped();
    let total_size = total_usable_memory(mf.total_size(), total_usage);
    let mem_free = total_size.saturating_sub(total_usage);
    let si = libc::sysinfo {
        uptime: ctx.now().seconds(),
        loads: [0; 3],
        totalram: total_size,
        freeram: mem_free,
        sharedram: 0,
        bufferram: 0,
        totalswap: 0,
        freeswap: 0,
        procs: 1,
        pad: 0,
        totalhigh: 0,
        freehigh: 0,
        mem_unit: 1,
        _f: [0; 0],
    };
    let b = unsafe {
        std::slice::from_raw_parts(
            &si as *const _ as *const u8,
            std::mem::size_of::<libc::sysinfo>(),
        )
    };
    let task = ctx.task();
    task.copy_out_bytes(addr, b).map(|_| 0)
}
