mod context;

pub use context::Context;

use std::sync::Mutex;

use mem::{AccessType, Addr, PAGE_SIZE};
use nix::sys::{
    ptrace,
    signal::Signal,
    wait::{waitpid, WaitStatus},
};
use once_cell::sync::{Lazy, OnceCell};
use utils::{FileRange, SysResult};

const STUB_INIT_ADDRESS: u64 = 0x7fffffff0000;
const MAX_USER_ADDRESS: u64 = 0x7ffffffff000; // largest possible user address

pub static STUB_START: Lazy<Mutex<u64>> = Lazy::new(|| Mutex::new(STUB_INIT_ADDRESS));
static STUB_END: OnceCell<u64> = OnceCell::new();

#[derive(Clone, Copy, Debug)]
pub enum Platform {
    Ptrace,
}

impl Platform {
    pub fn supports_address_space_io(&self) -> bool {
        false
    }
    pub fn map_unit(&self) -> u64 {
        0
    }
    pub fn min_user_address(&self) -> Addr {
        Addr(*SYSTEM_MMAP_MIN_ADDR.lock().unwrap())
    }
    // max_user_address returns the first address that may not be used by user application
    pub fn max_user_address(&self) -> Addr {
        Addr(*STUB_START.lock().unwrap())
    }
    pub fn new_address_space(&self, ctx: &dyn Context) -> PtraceAddressSpace {
        let stub_end = *STUB_END.get().unwrap();
        let address_space = PtraceAddressSpace;
        address_space.unmap(Addr(0), *STUB_START.lock().unwrap(), ctx);
        if stub_end != MAX_USER_ADDRESS {
            address_space.unmap(Addr(stub_end), MAX_USER_ADDRESS - stub_end, ctx);
        }
        address_space
    }
}

extern "C" {
    fn addr_of_stub() -> u64;
}

pub fn stub_init() -> anyhow::Result<()> {
    let stub_begin = unsafe { addr_of_stub() };
    // Just allocate a page size for now.
    let map_len = PAGE_SIZE as usize;
    while *STUB_START.lock().unwrap() > 0 {
        let stub_start = *STUB_START.lock().unwrap() as *mut libc::c_void;
        let addr = unsafe {
            libc::mmap(
                stub_start,
                map_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                0,
                0,
            )
        };
        if addr != stub_start {
            if addr != libc::MAP_FAILED && unsafe { libc::munmap(addr, map_len) } < 0 {
                panic!("munmap failed");
            }
            *STUB_START.lock().unwrap() -= PAGE_SIZE as u64;
            continue;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(stub_begin as *const u8, addr as *mut u8, map_len);
            if libc::mprotect(addr, map_len, libc::PROT_READ | libc::PROT_EXEC) < 0 {
                panic!("mprotect failed");
            }
        }
        let stub_end = stub_start as usize + map_len;
        STUB_END.set(stub_end as u64).unwrap();
        return Ok(());
    }
    anyhow::bail!("Failed to map stub")
}

pub static SYSTEM_MMAP_MIN_ADDR: Lazy<Mutex<u64>> = Lazy::new(|| {
    // TODO: better to read from the actual file once mounting /proc is completed
    // let addr: u64 = std::fs::read_to_string("/proc/sys/vm/mmap_min_addr")
    //     .expect("failed to read from \"/proc/sys/vm/mmap_min_addr\"")
    //     .trim()
    //     .parse()
    //     .expect("failed to parse");
    let addr = 65536;
    Mutex::new(addr)
});

pub fn create_syscall_regs(
    mut regs: libc::user_regs_struct,
    syscall_no: u64,
    args: &[u64],
) -> libc::user_regs_struct {
    regs.rax = syscall_no;
    if !args.is_empty() {
        regs.rdi = args[0];
    }
    if args.len() >= 2 {
        regs.rsi = args[1];
    }
    if args.len() >= 3 {
        regs.rdx = args[2];
    }
    if args.len() >= 4 {
        regs.r10 = args[3];
    }
    if args.len() >= 5 {
        regs.r8 = args[4];
    }
    if args.len() >= 6 {
        regs.r9 = args[5];
    }
    regs
}

#[derive(Debug)]
pub struct PtraceAddressSpace;

impl PtraceAddressSpace {
    pub fn map_file(
        &self,
        addr: Addr,
        fd: i32,
        fr: FileRange,
        at: AccessType,
        precommit: bool,
        ctx: &dyn Context,
    ) -> SysResult<()> {
        let flags = if precommit { libc::MAP_POPULATE } else { 0 };
        let pid = ctx.tid();
        logger::debug!("MapFile: from {} len {:#x}", addr, fr.len());
        let regs = create_syscall_regs(
            ctx.task_init_regs(),
            libc::SYS_mmap as u64,
            &[
                addr.0,
                fr.len(),
                at.as_prot() as u64,
                (flags | libc::MAP_SHARED | libc::MAP_FIXED) as u64,
                fd as u64,
                fr.start,
            ],
        );
        ctx.ptrace_set_regs(regs).expect("PTRACE_SETREGS failed");
        loop {
            ptrace::cont(pid, None).expect("PTRACE_CONT failed");
            match waitpid(pid, None).expect("wait failed") {
                WaitStatus::Stopped(_, sig) if sig == Signal::SIGSTOP => continue,
                WaitStatus::Stopped(_, sig) if sig == Signal::SIGTRAP => break,
                e => panic!("unhandled event {:?}", e),
            }
        }
        Ok(())
    }

    pub fn unmap(&self, addr: Addr, length: u64, ctx: &dyn Context) {
        let _ar = addr.to_range(length).expect("address overflows");
        logger::debug!("Unmap: from {} len {:#x}", addr, length);
        let pid = ctx.tid();
        let regs = create_syscall_regs(
            ctx.task_init_regs(),
            libc::SYS_munmap as u64,
            &[addr.0, length],
        );
        ctx.ptrace_set_regs(regs).expect("PTRACE_SETREGS failed");
        loop {
            ptrace::cont(pid, None).expect("PTRACE_CONT failed");
            match waitpid(pid, None).expect("wait failed") {
                WaitStatus::Stopped(_, sig) if sig == Signal::SIGSTOP => continue,
                WaitStatus::Stopped(_, sig) if sig == Signal::SIGTRAP => break,
                e => logger::debug!("received signal {:?}", e),
            }
        }
    }
    pub fn copy_in(&self, _: Addr, _: &mut [u8]) -> SysResult<usize> {
        unreachable!();
    }
    pub fn copy_out(&self, _: Addr, _: &[u8]) -> SysResult<usize> {
        unreachable!();
    }
    pub fn release(&self) {}
}
