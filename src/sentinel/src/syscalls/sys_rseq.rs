use utils::{err_libc, SysError};

// rseq implements linux syscall rseq(2)
pub fn rseq(_: &libc::user_regs_struct) -> super::Result {
    err_libc!(libc::ENOSYS)
}
