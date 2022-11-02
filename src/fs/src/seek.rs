use utils::{err_libc, SysError, SysResult};

#[derive(Debug)]
pub enum SeekWhence {
    Set,
    Current,
    End,
}

impl SeekWhence {
    pub fn from_linux(n: i32) -> SysResult<Self> {
        match n {
            0 => Ok(Self::Set),
            1 => Ok(Self::Current),
            2 => Ok(Self::End),
            _ => err_libc!(libc::EINVAL),
        }
    }
}
