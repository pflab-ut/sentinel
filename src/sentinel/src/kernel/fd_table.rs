use std::{cell::RefCell, collections::HashMap, rc::Rc};

use fs::{
    attr::{FileOwner, FilePermissions, PermMask, StableAttr, UnstableAttr},
    inode::Inode,
    mount::{MountSource, MountSourceFlags},
    tmpfs, Dirent, FdFlags, File, FileFlags,
};
use limit::Context as LimitContext;
use time::Context as TimeContext;
use usage::MemoryKind;
use utils::{bail_libc, SysError, SysResult};

use crate::context;

#[derive(Clone, Debug)]
struct Descriptor {
    file: Rc<RefCell<File>>,
    flags: FdFlags,
}

impl Descriptor {
    fn tty(fd: i32) -> Self {
        debug_assert!(fd == 0 || fd == 1 || fd == 2);
        let msrc = Rc::new(MountSource::new(MountSourceFlags::default()));
        let sattr = StableAttr::tty(fd)
            .unwrap_or_else(|_| panic!("failed to retrieve StableAttr form fd {}", fd));
        let perm_mask = PermMask {
            read: true,
            write: true,
            execute: false,
        };
        let uattr = UnstableAttr {
            size: 0,
            usage: 0,
            perms: FilePermissions {
                user: perm_mask,
                group: PermMask::default(),
                other: PermMask::default(),
                sticky: true,
                set_uid: false,
                set_gid: false,
            },
            owner: FileOwner::root(), //FIXME
            ..UnstableAttr::default()
        };
        let ctx = context::context();
        let uattr = uattr.record_current_time(|| ctx.now());
        let iops = Box::new(tmpfs::RegularFile::new_file_in_memory(
            MemoryKind::Tmpfs,
            uattr,
        ));
        let inode = Inode::new(iops, msrc, sattr);
        let inode_id = inode.stable_attr().inode_id;
        let dirent = Dirent::new(inode, format!("host[{}]", inode_id));
        let flags = FileFlags::from_fd(fd)
            .unwrap_or_else(|_| panic!("failed to retrieve FileFlags form fd {}", fd));
        let file = Rc::new(RefCell::new(File::new(
            flags,
            Box::new(tmpfs::RegularFileOperations { dirent }),
        )));
        Self {
            file,
            flags: FdFlags {
                close_on_exec: false,
            },
        }
    }
}

#[derive(Debug)]
pub struct FdTable {
    next: i32, // start position to find fd
    descriptor_table: HashMap<i32, Descriptor>,
    used: i32,
}

impl FdTable {
    pub fn init() -> Self {
        Self {
            next: 0,
            descriptor_table: HashMap::new(),
            used: 0,
        }
    }

    pub fn set_stdio_files(&mut self) {
        for fd in 0..=2 {
            self.descriptor_table.insert(fd, Descriptor::tty(fd));
        }
    }

    pub fn get(&self, fd: i32) -> Option<(Rc<RefCell<File>>, FdFlags)> {
        self.descriptor_table
            .get(&fd)
            .map(|d| (Rc::clone(&d.file), d.flags))
    }

    pub fn set(
        &mut self,
        fd: i32,
        file: Option<&Rc<RefCell<File>>>,
        flags: FdFlags,
    ) -> Option<Rc<RefCell<File>>> {
        let desc = file.map(|f| Descriptor {
            file: Rc::clone(f),
            flags,
        });
        let orig = match desc {
            Some(ref d) => self.descriptor_table.insert(fd, d.clone()),
            None => self.descriptor_table.remove(&fd),
        };

        if orig.is_none() && desc.is_some() {
            self.used += 1;
        } else if orig.is_some() && desc.is_none() {
            self.used -= 1;
        }

        if orig.as_ref().map_or(false, |o| {
            desc.as_ref()
                .map_or(true, |d| Rc::as_ptr(&o.file) == Rc::as_ptr(&d.file))
        }) {
            Some(orig.unwrap().file)
        } else {
            None
        }
    }

    pub fn new_fds(
        &mut self,
        fd: i32,
        files: &[&Rc<RefCell<File>>],
        flags: FdFlags,
    ) -> SysResult<Vec<i32>> {
        if fd < 0 {
            bail_libc!(libc::EINVAL);
        }

        let mut fds = Vec::new();

        let end = {
            let ctx = context::context();
            let lim = ctx.limits().get_number_of_files();
            if lim.cur != limit::INFINITY {
                lim.cur as i32
            } else {
                i32::MAX
            }
        };

        if fd + files.len() as i32 > end {
            bail_libc!(libc::EMFILE);
        }

        let fd = std::cmp::max(fd, self.next);

        for i in fd..end {
            if fds.len() >= files.len() {
                break;
            }
            if self.get(i).is_none() {
                self.set(i, files.get(fds.len()).copied(), flags);
                fds.push(i);
            }
        }

        if fds.len() < files.len() {
            // failed...
            for i in &fds {
                self.set(*i, None, FdFlags::default());
            }
            bail_libc!(libc::EMFILE);
        }

        if fd == self.next {
            self.next = fds.last().unwrap() + 1;
        }

        Ok(fds)
    }

    #[cfg(test)]
    pub fn new_fd_at(
        &mut self,
        fd: i32,
        file: &Rc<RefCell<File>>,
        flags: FdFlags,
    ) -> SysResult<()> {
        if fd < 0 {
            bail_libc!(libc::EBADF);
        }

        let ctx = context::context();
        let lim = ctx.limits().get_number_of_files();
        if lim.cur != limit::INFINITY && fd as u64 >= lim.cur {
            bail_libc!(libc::EMFILE);
        }
        self.set(fd, Some(file), flags);
        Ok(())
    }

    pub fn remove(&mut self, fd: i32) -> Option<Rc<RefCell<File>>> {
        if fd < 0 {
            return None;
        }
        if fd < self.next {
            self.next = fd;
        }
        let (orig, _) = self.get(fd)?;
        self.set(fd, None, FdFlags::default());
        Some(orig)
    }

    pub fn set_flags(&mut self, fd: i32, flags: FdFlags) -> SysResult<()> {
        if fd < 0 {
            bail_libc!(libc::EBADF);
        }
        let (file, _) = self.get(fd).ok_or_else(|| SysError::new(libc::EBADF))?;
        self.set(fd, Some(&file), flags);
        Ok(())
    }
}
