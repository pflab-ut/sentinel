use fs::seek::SeekWhence;
use utils::SysError;

use crate::context;

// lseek implements linux syscall lseek(2)
pub fn lseek(regs: &libc::user_regs_struct) -> super::Result {
    let fd = regs.rdi as i32;
    let offset = regs.rsi as i64;

    let whence = SeekWhence::from_linux(regs.rdx as i32)?;

    let ctx = context::context();
    let mut task = ctx.task_mut();
    let file = task
        .get_file(fd)
        .ok_or_else(|| SysError::new(libc::EBADF))?;
    let mut file = file.borrow_mut();
    file.seek(whence, offset).map(|p| p as usize)
}
