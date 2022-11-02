mod sys_epoll;
mod sys_eventfd;
mod sys_file;
mod sys_fscontext;
mod sys_futex;
mod sys_getdents;
mod sys_identity;
mod sys_lseek;
mod sys_mempolicy;
mod sys_mmap;
mod sys_pipe;
mod sys_poll;
mod sys_prctl;
mod sys_random;
mod sys_read;
mod sys_rlimit;
mod sys_rseq;
mod sys_signal;
mod sys_socket;
mod sys_stat;
mod sys_sysinfo;
mod sys_thread;
mod sys_thread_local_storage;
mod sys_time;
mod sys_timer;
mod sys_utsname;
mod sys_write;

use utils::SysError;

pub type Result = std::result::Result<usize, SysError>;

pub fn perform(regs: &mut libc::user_regs_struct, counter: usize) -> Result {
    logger::info!(
        "#{}: syscall {} with arguments: ({:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x})",
        counter,
        regs.orig_rax as i64,
        regs.rdi,
        regs.rsi,
        regs.rdx,
        regs.r10,
        regs.r8,
        regs.r9,
    );
    match regs.orig_rax as i64 {
        libc::SYS_read /* 0 */ => sys_read::read(regs),
        libc::SYS_write /* 1 */ => sys_write::write(regs),
        libc::SYS_open /* 2 */ => sys_file::open(regs),
        libc::SYS_close /* 3 */ => sys_file::close(regs),
        libc::SYS_stat /* 4 */ => sys_stat::stat(regs),
        libc::SYS_fstat /* 5 */ => sys_stat::fstat(regs),
        libc::SYS_lstat /* 6 */ => sys_stat::lstat(regs),
        libc::SYS_poll /* 7 */ => sys_poll::poll(regs),
        libc::SYS_lseek /* 8 */ => sys_lseek::lseek(regs),
        libc::SYS_mmap /* 9 */ => sys_mmap::mmap(regs),
        libc::SYS_mprotect /* 10 */ => sys_mmap::mprotect(regs),
        libc::SYS_munmap /* 11 */ => sys_mmap::munmap(regs),
        libc::SYS_brk /* 12 */ => sys_mmap::brk(regs),
        libc::SYS_rt_sigaction /* 13 */ => sys_signal::rt_sigaction(regs),
        libc::SYS_rt_sigprocmask /* 14 */ => sys_signal::rt_sigprocmask(regs),
        libc::SYS_ioctl /* 16 */ => sys_file::ioctl(regs),
        libc::SYS_pread64 /* 17 */ => sys_read::pread64(regs),
        libc::SYS_writev /* 20 */ => sys_write::writev(regs),
        libc::SYS_access /* 21 */ => sys_file::access(regs),
        libc::SYS_pipe /* 22 */ => sys_pipe::pipe(regs),
        libc::SYS_mremap /* 25 */ => sys_mmap::mremap(regs),
        libc::SYS_dup /* 32 */ => sys_file::dup(regs),
        libc::SYS_getpid /* 39 */ => sys_thread::getpid(regs),
        libc::SYS_socket /* 41 */ => sys_socket::socket(regs),
        libc::SYS_connect /* 42 */ => sys_socket::connect(regs),
        libc::SYS_accept /* 43 */ => sys_socket::accept(regs),
        libc::SYS_sendto /* 44 */ => sys_socket::sendto(regs),
        libc::SYS_recvfrom /* 45 */ => sys_socket::recvfrom(regs),
        libc::SYS_bind /* 49 */ => sys_socket::bind(regs),
        libc::SYS_listen /* 50 */ => sys_socket::listen(regs),
        libc::SYS_getsockname /* 51 */ => sys_socket::getsockname(regs),
        libc::SYS_getpeername /* 52 */ => sys_socket::getpeername(regs),
        libc::SYS_setsockopt /* 54 */ => sys_socket::setsockopt(regs),
        libc::SYS_getsockopt /* 55 */ => sys_socket::getsockopt(regs),
        libc::SYS_exit /* 60 */ => sys_thread::exit(regs),
        libc::SYS_uname /* 63 */ => sys_utsname::uname(regs),
        libc::SYS_fcntl /* 72 */ => sys_file::fcntl(regs),
        libc::SYS_getdents /* 78 */ => sys_getdents::getdents(regs),
        libc::SYS_getcwd /* 79 */ => sys_fscontext::getcwd(regs),
        libc::SYS_chdir /* 80 */ => sys_fscontext::chdir(regs),
        libc::SYS_rename /* 82 */ => sys_file::rename(regs),
        libc::SYS_readlink /* 89 */ => sys_file::readlink(regs),
        libc::SYS_sysinfo /* 99 */ => sys_sysinfo::sysinfo(regs),
        libc::SYS_getuid /* 102 */ => sys_identity::getuid(regs),
        libc::SYS_getgid /* 104 */ => sys_identity::getgid(regs),
        libc::SYS_geteuid /* 107 */ => sys_identity::geteuid(regs),
        libc::SYS_getegid /* 108 */ => sys_identity::getegid(regs),
        libc::SYS_sigaltstack /* 131 */ => sys_signal::sigaltstack(regs),
        libc::SYS_prctl /* 157 */ => sys_prctl::prctl(regs),
        libc::SYS_arch_prctl /* 158 */ => sys_thread_local_storage::arch_prctl(regs),
        libc::SYS_gettid /* 186 */ => sys_thread::gettid(regs),
        libc::SYS_futex /* 202 */ => sys_futex::futex(regs),
        libc::SYS_sched_getaffinity /* 204 */ => sys_thread::sched_getaffinity(regs),
        libc::SYS_getdents64 /* 217 */ => sys_getdents::getdents64(regs),
        libc::SYS_set_tid_address /* 218 */ => sys_thread::set_tid_address(regs),
        libc::SYS_timer_create /* 222 */ => sys_timer::timer_create(regs),
        libc::SYS_timer_delete /* 226 */ => sys_timer::timer_delete(regs),
        libc::SYS_clock_gettime /* 228 */ => sys_time::clock_gettime(regs),
        libc::SYS_clock_nanosleep /* 230 */ => sys_time::clock_nanosleep(regs),
        libc::SYS_exit_group /* 231 */ => sys_thread::exit_group(regs),
        libc::SYS_tgkill /* 234 */ => sys_signal::tgkill(regs),
        libc::SYS_mbind /* 237 */ => sys_mempolicy::mbind(regs),
        libc::SYS_openat /* 257 */ => sys_file::openat(regs),
        libc::SYS_newfstatat /* 262 */ => sys_stat::fstatat(regs),
        libc::SYS_renameat /* 264 */ => sys_file::renameat(regs),
        libc::SYS_set_robust_list /* 273 */ => sys_futex::set_robust_list(regs),
        libc::SYS_eventfd /* 284 */ => sys_eventfd::eventfd(*regs),
        libc::SYS_accept4 /* 288 */ => sys_socket::accept4(regs),
        libc::SYS_eventfd2 /* 290 */ => sys_eventfd::eventfd2(regs),
        libc::SYS_epoll_create1 /* 291 */ => sys_epoll::epoll_create1(regs),
        libc::SYS_pipe2 /* 293 */ => sys_pipe::pipe2(regs),
        libc::SYS_prlimit64 /* 302 */ => sys_rlimit::prlimit64(regs),
        libc::SYS_sendmmsg /* 307 */ => sys_socket::sendmmsg(regs),
        libc::SYS_getrandom /* 318 */ => sys_random::getrandom(regs),
        libc::SYS_rseq /* 334 */ => sys_rseq::rseq(regs),
        _ => {
            logger::info!("stdout: {:?}", crate::get_stdout());
            logger::info!("stderr: {:?}", crate::get_stderr());
            unimplemented!("syscall {} is not implemented", regs.orig_rax as i64);
        }
    }
}

pub fn should_exit(syscallno: i64) -> bool {
    matches!(syscallno, libc::SYS_exit | libc::SYS_exit_group)
}
