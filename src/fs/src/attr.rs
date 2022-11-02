use auth::id::{Kgid, Kuid};
use linux::{dev::make_device_id, FileMode};
use nix::{sys::stat, unistd, NixPath};
use time::Time;

use super::context::Context;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InodeType {
    RegularFile,
    SpecialFile,
    Directory,
    SpecialDirectory,
    Symlink,
    Pipe,
    Socket,
    CharacterDevice,
    BlockDevice,
    Anonymous,
}

impl InodeType {
    fn from_stat(stat: &stat::FileStat) -> Self {
        match stat.st_mode & libc::S_IFMT {
            libc::S_IFLNK => Self::Symlink,
            libc::S_IFIFO => Self::Pipe,
            libc::S_IFCHR => Self::CharacterDevice,
            libc::S_IFBLK => Self::BlockDevice,
            libc::S_IFSOCK => Self::Socket,
            libc::S_IFDIR => Self::Directory,
            libc::S_IFREG => Self::RegularFile,
            _ => panic!("unexpected stat.st_mode"),
        }
    }

    fn as_linux_type(&self) -> libc::mode_t {
        match *self {
            Self::RegularFile | Self::SpecialFile => libc::S_IFREG,
            Self::Directory | Self::SpecialDirectory => libc::S_IFDIR,
            Self::Symlink => libc::S_IFLNK,
            Self::Pipe => libc::S_IFIFO,
            Self::CharacterDevice => libc::S_IFCHR,
            Self::BlockDevice => libc::S_IFBLK,
            Self::Socket => libc::S_IFSOCK,
            Self::Anonymous => 0,
        }
    }

    pub fn as_dirent_type(&self) -> u8 {
        match *self {
            Self::RegularFile | Self::SpecialFile => libc::DT_REG,
            Self::Symlink => libc::DT_LNK,
            Self::Directory | Self::SpecialDirectory => libc::DT_DIR,
            Self::Pipe => libc::DT_FIFO,
            Self::CharacterDevice => libc::DT_CHR,
            Self::BlockDevice => libc::DT_BLK,
            Self::Socket => libc::DT_SOCK,
            Self::Anonymous => libc::DT_UNKNOWN,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StableAttr {
    pub typ: InodeType,
    pub device_id: u64,
    pub inode_id: u64,
    pub block_size: i64,
    pub device_file_major: u16,
    pub device_file_minor: u32,
}

impl StableAttr {
    pub fn from_fd(fd: i32) -> nix::Result<Self> {
        let stat = stat::fstat(fd)?;
        Ok(Self {
            typ: InodeType::from_stat(&stat),
            device_id: stat.st_dev,
            inode_id: stat.st_ino,
            block_size: stat.st_blksize,
            device_file_major: 0,
            device_file_minor: 0,
        })
    }

    pub fn from_path<P: ?Sized + NixPath>(path: &P) -> nix::Result<Self> {
        let stat = stat_from_path(path)?;
        Ok(Self {
            typ: InodeType::from_stat(&stat),
            device_id: stat.st_dev,
            inode_id: stat.st_ino,
            block_size: stat.st_blksize,
            device_file_major: 0,
            device_file_minor: 0,
        })
    }

    pub fn tty(fd: i32) -> nix::Result<Self> {
        let stat = stat::fstat(fd)?;
        Ok(Self {
            typ: InodeType::Pipe,
            device_id: stat.st_dev,
            inode_id: stat.st_ino,
            block_size: stat.st_blksize,
            device_file_major: 0,
            device_file_minor: 0,
        })
    }

    #[inline]
    pub fn is_regular(&self) -> bool {
        self.typ == InodeType::RegularFile
    }

    #[inline]
    pub fn is_directory(&self) -> bool {
        self.typ == InodeType::Directory || self.typ == InodeType::SpecialDirectory
    }

    #[inline]
    pub fn is_file(&self) -> bool {
        self.typ == InodeType::RegularFile || self.typ == InodeType::SpecialFile
    }

    #[inline]
    pub fn is_socket(&self) -> bool {
        self.typ == InodeType::Socket
    }

    #[inline]
    pub fn is_symlink(&self) -> bool {
        self.typ == InodeType::Symlink
    }

    #[inline]
    pub fn is_pipe(&self) -> bool {
        self.typ == InodeType::Pipe
    }

    #[inline]
    pub fn is_char_device(&self) -> bool {
        self.typ == InodeType::CharacterDevice
    }

    #[inline]
    pub fn is_block_device(&self) -> bool {
        self.typ == InodeType::BlockDevice
    }

    #[inline]
    pub fn would_block(&self) -> bool {
        matches!(
            self.typ,
            InodeType::Pipe | InodeType::Socket | InodeType::CharacterDevice,
        )
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct PermMask {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl PermMask {
    pub fn is_superset_of(&self, other: &Self) -> bool {
        if !self.read && other.read {
            false
        } else if !self.write && other.write {
            false
        } else {
            self.execute || !other.execute
        }
    }

    #[inline]
    pub fn is_read_only(&self) -> bool {
        self.read && !self.write && !self.execute
    }

    pub fn from_linux_flags(flags: u32) -> Self {
        let mut ret = Self::default();
        if flags as i32 & libc::O_TRUNC != 0 {
            ret.write = true;
        }
        match flags as i32 & libc::O_ACCMODE {
            libc::O_WRONLY => ret.write = true,
            libc::O_RDWR => {
                ret.write = true;
                ret.read = true;
            }
            libc::O_RDONLY => ret.read = true,
            _ => (),
        };
        ret
    }

    pub fn from_mode(mode: linux::FileMode) -> Self {
        Self {
            read: mode.0 & linux::MODE_OTHER_READ != 0,
            write: mode.0 & linux::MODE_OTHER_WRITE != 0,
            execute: mode.0 & linux::MODE_OTHER_EXEC != 0,
        }
    }

    pub fn mode(&self) -> u32 {
        let mut mode = 0;
        if self.read {
            mode |= libc::S_IROTH;
        }
        if self.write {
            mode |= libc::S_IWOTH;
        }
        if self.execute {
            mode |= libc::S_IXOTH;
        }
        mode
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct FilePermissions {
    pub user: PermMask,
    pub group: PermMask,
    pub other: PermMask,
    pub sticky: bool,
    pub set_uid: bool,
    pub set_gid: bool,
}

impl FilePermissions {
    pub fn any_execute(&self) -> bool {
        self.user.execute || self.group.execute || self.other.execute
    }

    pub fn from_mode(mode: linux::FileMode) -> Self {
        Self {
            user: PermMask::from_mode(mode >> 6),
            group: PermMask::from_mode(mode >> 3),
            other: PermMask::from_mode(mode),
            sticky: mode.0 as u32 & libc::S_ISVTX == libc::S_ISVTX,
            set_uid: mode.0 as u32 & libc::S_ISUID == libc::S_ISUID,
            set_gid: mode.0 as u32 & libc::S_ISGID == libc::S_ISGID,
        }
    }

    pub fn has_set_uid_or_gid(&self) -> bool {
        self.set_uid || self.set_gid
    }

    pub fn drop_set_uid_and_maybe_gid(&mut self) {
        self.set_uid = false;
        if self.group.execute {
            self.set_gid = false;
        }
    }

    pub fn as_linux_mode(&self) -> u32 {
        let mut m = self.user.mode() << 6 | self.group.mode() << 3 | self.other.mode();
        if self.set_uid {
            m |= libc::S_ISUID;
        }
        if self.set_gid {
            m |= libc::S_ISGID;
        }
        if self.sticky {
            m |= libc::S_ISVTX;
        }
        m
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct FileOwner {
    pub uid: Kuid,
    pub gid: Kgid,
}

impl FileOwner {
    fn from_stat(stat: &stat::FileStat) -> Self {
        Self {
            uid: Kuid(stat.st_uid),
            gid: Kgid(stat.st_gid),
        }
    }

    pub const fn root() -> Self {
        Self {
            uid: Kuid::root(),
            gid: Kgid::root(),
        }
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct UnstableAttr {
    pub size: i64,
    pub usage: i64,
    pub perms: FilePermissions,
    pub owner: FileOwner,
    pub access_time: Time,
    pub modification_time: Time,
    pub status_change_time: Time,
    pub links: u64,
}

impl UnstableAttr {
    pub fn record_current_time<F: Fn() -> Time>(mut self, time: F) -> Self {
        let t = time();
        self.access_time = t;
        self.modification_time = t;
        self.status_change_time = t;
        self
    }

    pub fn from_path<P: ?Sized + NixPath>(path: &P) -> nix::Result<Self> {
        let stat = stat_from_path(path)?;
        Ok(Self {
            size: stat.st_size,
            usage: stat.st_blocks * 512,
            perms: FilePermissions::from_mode(FileMode(stat.st_mode as u16)),
            owner: FileOwner::from_stat(&stat),
            access_time: Time::from_unix(stat.st_atime, stat.st_atime_nsec),
            modification_time: Time::from_unix(stat.st_mtime, stat.st_mtime_nsec),
            status_change_time: Time::from_unix(stat.st_ctime, stat.st_ctime_nsec),
            links: stat.st_nlink as u64,
        })
    }
}

#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
pub struct AttrMask {
    pub typ: bool,
    pub device_id: bool,
    pub inode_id: bool,
    pub block_size: bool,
    pub size: bool,
    pub usage: bool,
    pub perms: bool,
    pub uid: bool,
    pub gid: bool,
    pub access_time: bool,
    pub modification_time: bool,
    pub status_change_time: bool,
    pub links: bool,
}

impl AttrMask {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

pub fn stat_from_attrs(sattr: StableAttr, uattr: UnstableAttr, ctx: &dyn Context) -> libc::stat {
    let creds = ctx.credentials();
    let un = &creds.user_namespace;
    let a = uattr.access_time.as_libc_timespec();
    let m = uattr.modification_time.as_libc_timespec();
    let c = uattr.status_change_time.as_libc_timespec();

    let stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    let mut stat = unsafe { stat.assume_init() };
    stat.st_dev = sattr.device_id;
    stat.st_ino = sattr.inode_id;
    stat.st_nlink = uattr.links;
    stat.st_mode = sattr.typ.as_linux_type() | uattr.perms.as_linux_mode();
    stat.st_uid = un.map_from_kuid(&uattr.owner.uid).or_overflow().0;
    stat.st_gid = un.map_from_kgid(&uattr.owner.gid).or_overflow().0;
    stat.st_rdev = make_device_id(sattr.device_file_major, sattr.device_file_minor) as u64;
    stat.st_size = uattr.size;
    stat.st_blksize = sattr.block_size;
    stat.st_blocks = uattr.usage / 512;
    stat.st_atime = a.tv_sec;
    stat.st_atime_nsec = a.tv_nsec;
    stat.st_mtime = m.tv_sec;
    stat.st_mtime_nsec = m.tv_nsec;
    stat.st_ctime = c.tv_sec;
    stat.st_ctime_nsec = c.tv_nsec;

    stat
}

fn stat_from_path<P: ?Sized + NixPath>(path: &P) -> nix::Result<stat::FileStat> {
    let fd = path.with_nix_path(|cstr| unsafe { libc::open(cstr.as_ptr(), libc::O_RDONLY) })?;
    let res = stat::fstat(fd);
    unistd::close(fd)?;
    res
}
