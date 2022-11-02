#[derive(Copy, Clone, Default, Debug)]
pub struct FdFlags {
    pub close_on_exec: bool,
}

impl FdFlags {
    pub fn as_linux_fd_flags(&self) -> i32 {
        if self.close_on_exec {
            libc::FD_CLOEXEC
        } else {
            0
        }
    }
}
