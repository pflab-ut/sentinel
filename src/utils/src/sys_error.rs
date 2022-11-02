use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SysErrorKind {
    Libc,
    Nix, // from nix crate
    BusError,
    Eof,
    ExceedsFileSizeLimit,
    ErrWouldBlock,
    SegFault(u64),
    SyscallRestart,
    ErrResolveViaReadLink,
    StdIoError,
    Smoltcp,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SysError {
    code: i32,
    desc: Option<String>,
    kind: SysErrorKind,
}

impl fmt::Display for SysError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SysError occured with code: {} {:?} {:?}",
            self.code, self.desc, self.kind
        )
    }
}

impl std::error::Error for SysError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

impl SysError {
    pub fn new(code: i32) -> Self {
        Self {
            code,
            desc: None,
            kind: SysErrorKind::Libc,
        }
    }

    pub fn new_with_msg(code: i32, msg: String) -> Self {
        Self {
            code,
            desc: Some(msg),
            kind: SysErrorKind::Libc,
        }
    }

    pub fn new_bus_error(code: i32) -> Self {
        Self {
            code,
            desc: None,
            kind: SysErrorKind::BusError,
        }
    }

    pub fn resolve_via_readlink() -> Self {
        Self {
            code: libc::EINVAL,
            desc: Some("link should be resolved via Readlink()".to_string()),
            kind: SysErrorKind::ErrResolveViaReadLink,
        }
    }

    pub fn kind(&self) -> SysErrorKind {
        self.kind
    }

    pub fn code(&self) -> i32 {
        self.code
    }

    pub fn eof() -> Self {
        Self {
            code: -1,
            desc: Some("EOF".to_string()),
            kind: SysErrorKind::Eof,
        }
    }

    pub fn exceeds_file_size_limit() -> Self {
        Self {
            code: -1,
            desc: Some("exceeds file size limit".to_string()),
            kind: SysErrorKind::ExceedsFileSizeLimit,
        }
    }

    pub fn erestartsys() -> Self {
        Self {
            code: 512,
            desc: None,
            kind: SysErrorKind::SyscallRestart,
        }
    }

    pub fn from_io_error(e: std::io::Error) -> Self {
        Self {
            code: e.raw_os_error().unwrap_or(-1),
            desc: None,
            kind: SysErrorKind::StdIoError,
        }
    }

    pub fn from_nix_errno(e: nix::errno::Errno) -> Self {
        Self {
            code: nix::errno::errno(),
            desc: Some(e.desc().to_string()),
            kind: SysErrorKind::Nix,
        }
    }

    pub fn from_smoltcp_error(e: smoltcp::Error) -> Self {
        // FIXME: should provide proper error code
        Self {
            code: -1,
            desc: Some(format!("Error from smoltcp {:?}", e)),
            kind: SysErrorKind::Smoltcp,
        }
    }
}

#[macro_export]
macro_rules! err_libc {
    ($libc_code:expr) => {
        Err(SysError::new($libc_code))
    };
}

#[macro_export]
macro_rules! bail_libc {
    ($libc_code:expr) => {
        return Err(SysError::new($libc_code))
    };
}

pub type SysResult<T> = std::result::Result<T, SysError>;
