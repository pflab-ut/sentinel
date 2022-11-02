use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::Path,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};
use utils::{bail_libc, SysError, SysResult};

use arch::{
    signal::{SignalStack, SIGNAL_STACK_FLAG_DISABLE, SIGNAL_STACK_FLAG_ON_STACK},
    ArchContext, CPUID_INSTRUCTION,
};
use fs::{mount::MountNamespace, FdFlags, File};
use mem::{copy_string_in, io::Io, Addr, AddrRangeSeq, IoOpts, IoSequence};
use nix::sys::ptrace;
use platform::{Context, PtraceAddressSpace};

use crate::{context, mm::MemoryManager};

use super::{
    fd_table::FdTable,
    task_image::{MemoryManagerState, TaskImage},
    UtsNameSpace,
};

const MAX_RW_COUNT: u64 = Addr(i32::MAX as u64).round_down().0;
static IOVEC_SIZE: usize = std::mem::size_of::<libc::iovec>();

#[derive(Debug)]
pub struct ExitStatus {
    pub code: i32,
    pub sig_no: i32,
}

#[derive(Debug)]
pub struct Task {
    fd_table: FdTable,
    image: TaskImage,
    exiting: bool,
    exit_status: Option<ExitStatus>,
    mounts: MountNamespace,
    robust_list: Addr,
    signal_mask: AtomicU64,
    signal_stack: SignalStack,
    clear_tid: Addr,
    arch_context: Option<ArchContext>,
    init_regs: libc::user_regs_struct,
    uts_namespace: UtsNameSpace,
    cpu_mask: Vec<u8>,
    parent_death_signal: linux::Signal,
    next_timerid: i32,
    timers: HashSet<i32>, //FIXME: properly implement timer instead of just holding the id
    signal_handlers: HashMap<linux::Signal, linux::SigAction>,
}

unsafe impl Send for Task {}
unsafe impl Sync for Task {}

impl Task {
    pub fn new(mounts: MountNamespace) -> anyhow::Result<Self> {
        let image = TaskImage::new();

        // Allow only 1 cpu.
        let mut cpu_mask = vec![0; 8];
        cpu_mask[0] = 1;

        Ok(Self {
            fd_table: FdTable::init(),
            image,
            exiting: false,
            exit_status: None,
            mounts,
            robust_list: Addr(0),
            signal_mask: AtomicU64::new(linux::SignalSet::default()),
            signal_stack: SignalStack::default(),
            clear_tid: Addr(0),
            arch_context: None,
            init_regs: utils::init_libc_regs(),
            uts_namespace: UtsNameSpace::new("sentinel".to_string(), "sentinel".to_string()),
            cpu_mask,
            parent_death_signal: linux::Signal(0),
            next_timerid: 0,
            timers: HashSet::new(),
            signal_handlers: HashMap::new(),
        })
    }

    pub fn load<P: AsRef<Path>>(
        &mut self,
        executable_path: P,
        argv: Vec<String>,
        envv: &HashMap<String, String>,
        extra_auxv: &HashMap<u64, Addr>,
    ) -> anyhow::Result<ArchContext> {
        self.fd_table.set_stdio_files();
        self.image
            .load(executable_path, argv, envv, extra_auxv, &self.mounts)
    }

    pub fn set_address_space(&self, address_space: PtraceAddressSpace) {
        self.image.set_address_space(address_space)
    }

    pub fn get_file(&mut self, fd: i32) -> Option<Rc<RefCell<File>>> {
        self.fd_table.get(fd).map(|(f, _)| f)
    }

    pub fn get_file_and_fd_flags(&mut self, fd: i32) -> Option<(Rc<RefCell<File>>, FdFlags)> {
        self.fd_table.get(fd)
    }

    #[inline]
    pub fn set_exit_status(&mut self, exit_status: ExitStatus) {
        self.exit_status = Some(exit_status);
    }

    #[inline]
    pub fn regs(&self) -> libc::user_regs_struct {
        self.arch_context
            .as_ref()
            .expect("ArchContext is not set yet")
            .regs
    }

    #[inline]
    pub fn set_regs(&mut self, regs: libc::user_regs_struct) {
        self.arch_context
            .as_mut()
            .expect("ArchContext is not set yet")
            .regs = regs;
    }

    #[inline]
    pub fn set_arch_context(&mut self, arch_context: ArchContext) {
        self.arch_context = Some(arch_context);
    }

    pub fn grab_init_regs(&mut self) {
        let ctx = &*context::context();
        let pid = ctx.tid();
        let mut regs = ptrace::getregs(pid).expect("PTRACE_GETREGS failed");
        regs.rip -= 2;
        logger::debug!("grab init regs: {:?}", regs);
        self.init_regs = regs;
    }

    #[inline]
    pub fn init_regs(&self) -> libc::user_regs_struct {
        self.init_regs
    }

    #[inline]
    pub fn cpu_mask(&self) -> &[u8] {
        &self.cpu_mask
    }

    pub fn ptrace_get_regs(
        &self,
        regs: &mut libc::user_regs_struct,
        pid: nix::unistd::Pid,
    ) -> nix::Result<()> {
        *regs = ptrace::getregs(pid)?;
        // logger::trace!("PTRACE_GETREGS: {:?}", *regs);
        Ok(())
    }

    pub fn reset_sysemu_regs(&mut self, regs: &mut libc::user_regs_struct) {
        regs.cs = self.init_regs.cs;
        regs.ss = self.init_regs.ss;
        regs.ds = self.init_regs.ds;
        regs.es = self.init_regs.es;
        regs.fs = self.init_regs.fs;
        regs.gs = self.init_regs.gs;
    }

    pub fn arch_context_mut(&mut self) -> &mut ArchContext {
        self.arch_context.as_mut().expect("ArchContext is not set")
    }

    pub fn handle_possible_cpuid_instruction(&mut self) -> bool {
        let mut current_inst = vec![0; CPUID_INSTRUCTION.len()];
        let res = self.memory_manager().borrow_mut().copy_in(
            Addr(self.regs().rip),
            &mut current_inst,
            &IoOpts {
                ignore_permissions: true,
            },
        );
        if res.is_err() || current_inst != CPUID_INSTRUCTION {
            false
        } else {
            // This is cpuid instruction. Handle this!
            self.arch_context_mut().cpuid_emulate();
            self.arch_context_mut().regs.rip += CPUID_INSTRUCTION.len() as u64;
            logger::error!("calling cpuid instruction");
            true
        }
    }

    #[inline]
    pub fn fd_table_mut(&mut self) -> &mut FdTable {
        &mut self.fd_table
    }

    #[inline]
    pub fn signal_mask(&self) -> linux::SignalSet {
        self.signal_mask.load(Ordering::SeqCst)
    }

    pub fn set_signal_mask(&self, mask: linux::SignalSet) {
        let _old_mask = self.signal_mask.swap(mask, Ordering::SeqCst);
        // TODO: handle the case where the new mask blocks some signals that were not blocked by the old
        // mask, and the case where the new mask unblocks some signals that were blocked by the old
        // mask.
    }

    fn on_signal_stack(&self, alt: &SignalStack) -> bool {
        let sp = Addr(self.regs().rsp);
        alt.contains(sp)
    }

    pub fn signal_stack(&self) -> SignalStack {
        let mut alt = self.signal_stack;
        if self.on_signal_stack(&alt) {
            alt.flags |= SIGNAL_STACK_FLAG_ON_STACK;
        }
        alt
    }

    pub fn set_signal_stack(&mut self, mut alt: SignalStack) -> bool {
        if self.on_signal_stack(&self.signal_stack) {
            return false;
        }
        if alt.flags & SIGNAL_STACK_FLAG_DISABLE != 0 {
            self.signal_stack = SignalStack {
                flags: SIGNAL_STACK_FLAG_DISABLE,
                ..SignalStack::default()
            };
        } else {
            alt.flags &= SIGNAL_STACK_FLAG_DISABLE;
            self.signal_stack = alt;
        }
        true
    }

    #[inline]
    pub fn set_clear_tid(&mut self, tid: Addr) {
        self.clear_tid = tid;
    }

    pub fn copy_in_sig_set(&self, sigset_addr: Addr, size: i32) -> SysResult<linux::SignalSet> {
        if size != linux::SIGNAL_SET_SIZE {
            bail_libc!(libc::EINVAL);
        }
        let mut buf = [0; 8];
        self.copy_in_bytes(sigset_addr, &mut buf)?;
        let mask = u64::from_le_bytes(buf);
        Ok(mask & !(linux::Signal::unblocked().0 as u64))
    }

    pub fn copy_out_sig_set(&self, sigset_addr: Addr, mask: linux::SignalSet) -> SysResult<()> {
        let b = mask.to_le_bytes();
        self.copy_out_bytes(sigset_addr, &b)?;
        Ok(())
    }

    pub fn copy_out_signal_stack(&self, addr: Addr, signal_stack: &SignalStack) -> SysResult<()> {
        let signal_stack_bytes = unsafe { signal_stack.as_bytes() };
        self.copy_out_bytes(addr, signal_stack_bytes).map(|_| ())
    }

    pub fn copy_in_signal_stack(&self, addr: Addr) -> SysResult<SignalStack> {
        let mut buf = vec![0; std::mem::size_of::<SignalStack>()];
        self.copy_in_bytes(addr, &mut buf)?;
        Ok(unsafe { SignalStack::from_bytes(&buf) })
    }

    pub fn copy_in_iovecs(&self, mut addr: Addr, num_iovecs: usize) -> SysResult<AddrRangeSeq> {
        if num_iovecs == 0 {
            return Ok(AddrRangeSeq::default());
        }
        let mut dst = Vec::with_capacity(num_iovecs);

        addr.add_length((num_iovecs * IOVEC_SIZE) as u64)
            .ok_or_else(|| SysError::new(libc::EFAULT))?;
        let mut buf = vec![0; IOVEC_SIZE];
        for _ in 0..num_iovecs {
            self.copy_in_bytes(addr, &mut buf)?;
            let iovec = unsafe { *(buf.as_ptr() as *const libc::iovec) };
            let base = Addr(iovec.iov_base as u64);
            let length = iovec.iov_len;
            if length > i64::MAX as usize {
                bail_libc!(libc::EINVAL);
            }
            let ar = self
                .memory_manager()
                .borrow()
                .check_io_range(base, length as i64)
                .ok_or_else(|| SysError::new(libc::EINVAL))?;
            if num_iovecs == 1 {
                let mut ar = AddrRangeSeq::from(ar);
                ar.truncate_to_first(MAX_RW_COUNT as usize);
                return Ok(ar);
            }
            dst.push(ar);
            addr += Addr(IOVEC_SIZE as u64);
        }

        let (_, dst) = dst.iter().fold((0, vec![]), |(sum, mut ds), d| {
            let mut dst_len = d.len();
            let rem = MAX_RW_COUNT - sum;
            let mut nxt = *d;
            if rem < dst_len {
                nxt.end -= dst_len - rem;
                dst_len = rem;
            }
            ds.push(nxt);
            (sum + dst_len, ds)
        });
        Ok(AddrRangeSeq::from_slice(&dst))
    }

    pub fn single_io_sequence(
        &self,
        addr: Addr,
        length: i32,
        opts: IoOpts,
    ) -> SysResult<IoSequence> {
        let length = std::cmp::min(length, MAX_RW_COUNT as i32);
        let memory_manager = self.memory_manager();
        let range = memory_manager
            .as_ref()
            .borrow_mut()
            .check_io_range(addr, length as i64)
            .ok_or_else(|| SysError::new(libc::EFAULT))?;
        Ok(IoSequence {
            io: memory_manager.clone(),
            addrs: AddrRangeSeq::from(range),
            opts,
        })
    }

    #[inline]
    pub fn mount_namespace(&self) -> &MountNamespace {
        &self.mounts
    }

    #[inline]
    pub fn uts_namespace(&self) -> &UtsNameSpace {
        &self.uts_namespace
    }

    pub fn new_fd_from(
        &mut self,
        fd: i32,
        file: &Rc<RefCell<File>>,
        flags: FdFlags,
    ) -> SysResult<i32> {
        self.fd_table.new_fds(fd, &[file], flags).map(|fds| fds[0])
    }

    #[inline]
    pub fn fd_table(&self) -> &FdTable {
        &self.fd_table
    }

    #[inline]
    pub fn set_robust_list(&mut self, addr: Addr) {
        self.robust_list = addr;
    }

    #[inline]
    pub fn memory_manager(&self) -> &Rc<RefCell<MemoryManager>> {
        match self.image.memory_manager {
            MemoryManagerState::Loaded(ref mm) => mm,
            MemoryManagerState::Empty => panic!("MemoryManager is not loaded yet!"),
        }
    }

    pub fn copy_in_string(&mut self, addr: Addr, max_len: usize) -> SysResult<String> {
        copy_string_in(
            self.memory_manager(),
            addr,
            max_len,
            &IoOpts {
                ignore_permissions: false,
            },
        )
    }

    pub fn copy_out_bytes(&self, addr: Addr, src: &[u8]) -> SysResult<usize> {
        let opts = IoOpts {
            ignore_permissions: false,
        };
        self.memory_manager()
            .borrow_mut()
            .copy_out(addr, src, &opts)
    }

    pub fn copy_in_bytes(&self, addr: Addr, dst: &mut [u8]) -> SysResult<usize> {
        let opts = IoOpts {
            ignore_permissions: false,
        };
        self.memory_manager().borrow_mut().copy_in(addr, dst, &opts)
    }

    pub fn prepare_group_exit(&mut self, exit_status: ExitStatus) {
        self.exiting = true;
        self.exit_status = Some(exit_status);
    }

    pub fn set_sigaction(
        &mut self,
        sig: linux::Signal,
        action: Option<linux::SigAction>,
    ) -> SysResult<linux::SigAction> {
        if !sig.is_valid() {
            bail_libc!(libc::EINVAL);
        }
        let signal_handlers = &mut self.signal_handlers;
        let old_act = signal_handlers.get(&sig).copied().unwrap_or_default();
        if let Some(mut action) = action {
            if sig.0 == libc::SIGKILL || sig.0 == libc::SIGSTOP {
                bail_libc!(libc::EINVAL);
            }
            action.mask &= !(linux::Signal::unblocked().0 as u64);
            signal_handlers.insert(sig, action);
        }
        Ok(old_act)
    }

    pub fn iovecs_io_sequence(
        &self,
        addr: Addr,
        iov_count: i32,
        opts: IoOpts,
    ) -> SysResult<IoSequence> {
        if !(0..=libc::UIO_MAXIOV).contains(&iov_count) {
            bail_libc!(libc::EINVAL);
        }
        let addrs = self.copy_in_iovecs(addr, iov_count as usize)?;
        Ok(IoSequence {
            io: self.memory_manager().clone(),
            addrs,
            opts,
        })
    }

    #[inline]
    pub fn parent_death_signal(&self) -> linux::Signal {
        self.parent_death_signal
    }

    #[inline]
    pub fn set_parent_death_signal(&mut self, signal: linux::Signal) {
        self.parent_death_signal = signal;
    }

    pub fn create_timer(&mut self) -> i32 {
        let ret = self.next_timerid;
        self.timers.insert(ret);
        self.next_timerid += 1;
        ret
    }

    pub fn delete_timer(&mut self, id: i32) -> bool {
        self.timers.remove(&id)
    }
}

#[cfg(test)]
mod tests {
    use crate::context;
    use fs::file_test_utils::new_test_file;
    use limit::{Limit, LimitSet};

    use super::*;

    const MAX_FD: u64 = 2 * 1024;

    fn run_test(f: fn(&mut FdTable, Rc<RefCell<File>>)) {
        context::init_for_test();
        let limit_set = {
            let mut limit_set = LimitSet::default();
            limit_set
                .set_number_of_files(
                    Limit {
                        cur: MAX_FD,
                        max: MAX_FD,
                    },
                    true,
                )
                .unwrap();
            limit_set
        };

        {
            let mut ctx = context::context_mut();
            ctx.set_limits(limit_set);
        }

        let mut fd_table = FdTable::init();

        let file = {
            let ctx = context::context();
            let file = new_test_file(&*ctx);
            Rc::new(RefCell::new(file))
        };
        f(&mut fd_table, file);
    }

    #[test]
    fn fd_table_many() {
        run_test(|fd_table, file| {
            for _ in 0..MAX_FD {
                assert!(fd_table.new_fds(0, &[&file], FdFlags::default()).is_ok());
            }
            assert!(fd_table.new_fds(0, &[&file], FdFlags::default()).is_err());
            assert!(fd_table.new_fd_at(1, &file, FdFlags::default()).is_ok());
            let i = 2;
            fd_table.remove(i);
            let fds = fd_table.new_fds(0, &[&file], FdFlags::default());
            assert!(fds.is_ok());
            assert_eq!(fds.unwrap()[0], i);
        });
    }

    #[test]
    fn fd_table_over_limit() {
        run_test(|fd_table, file| {
            assert!(fd_table
                .new_fds(MAX_FD as i32, &[&file], FdFlags::default())
                .is_err());
            assert!(fd_table
                .new_fds(
                    MAX_FD as i32 - 2,
                    &[&file, &file, &file],
                    FdFlags::default()
                )
                .is_err());
            let res = fd_table.new_fds(
                MAX_FD as i32 - 3,
                &[&file, &file, &file],
                FdFlags::default(),
            );
            assert!(res.is_ok());
            for fd in res.unwrap() {
                fd_table.remove(fd);
            }
            let res = fd_table.new_fds(MAX_FD as i32 - 1, &[&file], FdFlags::default());
            assert!(res.is_ok());
            assert_eq!(res.unwrap()[0], MAX_FD as i32 - 1);
            let res = fd_table.new_fds(0, &[&file], FdFlags::default());
            assert!(res.is_ok());
            assert_eq!(res.unwrap(), vec![0]);
        })
    }

    #[test]
    fn fd_table() {
        run_test(|fd_table, file| {
            {
                let ctx = context::context();
                let mut limit_set = ctx.limits_mut();
                limit_set
                    .set_number_of_files(
                        Limit {
                            cur: 1,
                            max: MAX_FD,
                        },
                        true,
                    )
                    .unwrap();
            }

            assert!(fd_table.new_fds(0, &[&file], FdFlags::default()).is_ok());
            assert!(fd_table.new_fds(0, &[&file], FdFlags::default()).is_err());

            {
                let ctx = context::context();
                let mut limit_set = ctx.limits_mut();
                limit_set
                    .set_number_of_files(
                        Limit {
                            cur: MAX_FD,
                            max: MAX_FD,
                        },
                        true,
                    )
                    .unwrap();
            }

            let res = fd_table.new_fds(0, &[&file], FdFlags::default());
            assert!(res.is_ok());
            assert_eq!(res.unwrap(), vec![1]);

            assert!(fd_table.new_fd_at(1, &file, FdFlags::default()).is_ok());
            assert!(fd_table
                .new_fd_at(MAX_FD as i32 + 1, &file, FdFlags::default())
                .is_err());
            assert!(fd_table.get(1).is_some());
            assert!(fd_table.get(2).is_none());
            let rm = fd_table.remove(1);
            assert!(rm.is_some());
            assert!(fd_table.remove(1).is_none());
        });
    }

    #[test]
    fn descriptor_flags() {
        run_test(|fd_table, file| {
            assert!(fd_table
                .new_fd_at(
                    2,
                    &file,
                    FdFlags {
                        close_on_exec: true
                    }
                )
                .is_ok());
            let res = fd_table.get(2);
            assert!(res.is_some());
            let (_, flags) = res.unwrap();
            assert!(flags.close_on_exec);
        });
    }
}
