use goblin::elf64::program_header::{PF_R, PF_W, PF_X};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct AccessType {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl std::fmt::Debug for AccessType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}{}{}",
            if self.read { "r" } else { "-" },
            if self.write { "w" } else { "-" },
            if self.execute { "x" } else { "-" }
        )
    }
}

impl AccessType {
    pub const fn no_access() -> Self {
        Self {
            read: false,
            write: false,
            execute: false,
        }
    }

    pub const fn read() -> Self {
        Self {
            read: true,
            write: false,
            execute: false,
        }
    }

    pub const fn write() -> Self {
        Self {
            read: false,
            write: true,
            execute: false,
        }
    }

    pub const fn execute() -> Self {
        Self {
            read: false,
            write: false,
            execute: true,
        }
    }

    pub const fn read_write() -> Self {
        Self {
            read: true,
            write: true,
            execute: false,
        }
    }

    pub const fn any_access() -> Self {
        Self {
            read: true,
            write: true,
            execute: true,
        }
    }

    pub fn is_superset_of(&self, other: AccessType) -> bool {
        if !self.read && other.read {
            false
        } else if !self.write && other.write {
            false
        } else {
            self.execute || !other.execute
        }
    }

    pub fn intersect(&self, other: AccessType) -> AccessType {
        AccessType {
            read: self.read && other.read,
            write: self.write && other.write,
            execute: self.execute && other.execute,
        }
    }

    pub fn union(&self, other: AccessType) -> AccessType {
        AccessType {
            read: self.read || other.read,
            write: self.write || other.write,
            execute: self.execute || other.execute,
        }
    }

    pub fn effective(&self) -> AccessType {
        let mut ret = *self;
        if ret.write || ret.execute {
            ret.read = true;
        }
        ret
    }

    pub fn any(&self) -> bool {
        self.read || self.write || self.execute
    }

    pub fn from_elf_prog_flags(flags: u32) -> Self {
        Self {
            read: flags & PF_R == PF_R,
            write: flags & PF_W == PF_W,
            execute: flags & PF_X == PF_X,
        }
    }

    pub fn as_prot(&self) -> i32 {
        let mut prot = 0;
        if self.read {
            prot |= libc::PROT_READ;
        }
        if self.write {
            prot |= libc::PROT_WRITE;
        }
        if self.execute {
            prot |= libc::PROT_EXEC;
        }
        prot
    }

    pub fn from_prot(prot: i32) -> Self {
        Self {
            read: prot & libc::PROT_READ != 0,
            write: prot & libc::PROT_WRITE != 0,
            execute: prot & libc::PROT_EXEC != 0,
        }
    }
}
