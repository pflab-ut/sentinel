use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};

use mem::{Addr, IoOpts, IoSequence};
use utils::SysResult;

use crate::{inode::Inode, Dirent, FdFlags, File};

use super::attr::{FileOwner, PermMask};

#[cfg(test)]
use auth::credentials::Credentials;
#[cfg(test)]
use limit::LimitSet;
#[cfg(test)]
#[cfg(test)]
use pgalloc::{MemoryFile, MemoryFileOpts, MemoryFileProvider};
#[cfg(test)]
use smoltcp::{
    iface::{Interface, SocketHandle},
    phy::TunTapInterface,
    socket::Socket,
    time::Duration,
};
#[cfg(test)]
use std::{
    fs::File as StdFile,
    os::unix::prelude::FromRawFd,
    sync::{RwLock, RwLockReadGuard, RwLockWriteGuard},
};
#[cfg(test)]
use time::{Clock, HostClock, Time};

pub trait Context:
    auth::Context
    + mem::Context
    + time::Context
    + limit::Context
    + memmap::Context
    + pgalloc::Context
    + net::Context
{
    fn working_directory(&self) -> &DirentRef;
    fn root_directory(&self) -> &DirentRef;
    fn umask(&self) -> u32;
    fn can_access_file(&self, inode: &Inode, p: PermMask) -> bool;
    fn file_owner(&self) -> FileOwner;

    // TODO: feels weird to place these methods here..
    fn single_io_sequence(&self, addr: Addr, length: i32, opts: IoOpts) -> SysResult<IoSequence>;
    fn new_fd_from(&self, fd: i32, file: &Rc<RefCell<File>>, flags: FdFlags) -> SysResult<i32>;
}

pub type DirentRef = Rc<RefCell<Dirent>>;
pub type DirentWeakRef = Weak<RefCell<Dirent>>;

#[derive(Debug)]
pub struct FsContext {
    root: Option<DirentRef>,
    cwd: Option<DirentRef>,
    umask: u32,
}

impl FsContext {
    pub fn new(root: Option<DirentRef>, cwd: Option<DirentRef>, umask: u32) -> Self {
        Self { root, cwd, umask }
    }

    pub fn working_directory(&self) -> &DirentRef {
        self.cwd.as_ref().unwrap()
    }

    pub fn set_working_directory(&mut self, dir: DirentRef) {
        self.cwd = Some(dir);
    }

    pub fn root_directory(&self) -> &DirentRef {
        self.root.as_ref().unwrap()
    }

    pub fn umask(&self) -> u32 {
        self.umask
    }
}

#[cfg(test)]
pub struct TestContext {
    credentials: Credentials,
    fs_context: FsContext,
    limits: LimitSet,
    mfp: TestMemoryFileProvider,
}

#[cfg(test)]
impl TestContext {
    pub fn init() -> Self {
        let limits = LimitSet::default();
        let credentials = Credentials::new_anonymous();
        let fs_context = FsContext {
            root: None,
            cwd: None,
            umask: 0o22,
        };
        let mfp = TestMemoryFileProvider::new();
        Self {
            credentials,
            fs_context,
            limits,
            mfp,
        }
    }
}

#[cfg(test)]
impl auth::Context for TestContext {
    fn credentials(&self) -> &Credentials {
        &self.credentials
    }
}

#[cfg(test)]
impl mem::Context for TestContext {
    fn copy_in_bytes(&self, _: Addr, _: &mut [u8]) -> SysResult<usize> {
        unimplemented!()
    }
    fn copy_out_bytes(&self, _: Addr, _: &[u8]) -> SysResult<usize> {
        unimplemented!()
    }
}

#[cfg(test)]
impl time::Context for TestContext {
    fn now(&self) -> Time {
        HostClock.now()
    }
}

#[cfg(test)]
impl limit::Context for TestContext {
    fn limits(&self) -> LimitSet {
        self.limits
    }
}

#[cfg(test)]
impl pgalloc::Context for TestContext {
    fn memory_file_provider(&self) -> &dyn pgalloc::MemoryFileProvider {
        &self.mfp
    }
}

#[cfg(test)]
impl memmap::Context for TestContext {
    fn mm(&self) -> Rc<RefCell<dyn memmap::MemoryInvalidator>> {
        unimplemented!()
    }
}

#[cfg(test)]
impl net::Context for TestContext {
    fn add_socket(&self, _socket: Socket<'static>) -> SocketHandle {
        unimplemented!()
    }
    fn poll_wait(&self, _once: bool) {
        unimplemented!()
    }
    fn gen_local_port(&self) -> u16 {
        unimplemented!()
    }
    fn remove_local_port(&self, _p: u16) {
        unimplemented!()
    }
    fn wait(&self, _duration: Option<Duration>) {
        unimplemented!()
    }
    fn network_interface_mut(&self) -> RwLockWriteGuard<'_, Interface<'static, TunTapInterface>> {
        unimplemented!()
    }
    fn as_net_context(&self) -> &dyn net::Context {
        self
    }
}

#[cfg(test)]
impl Context for TestContext {
    fn working_directory(&self) -> &DirentRef {
        self.fs_context.working_directory()
    }
    fn root_directory(&self) -> &DirentRef {
        self.fs_context.root_directory()
    }
    fn umask(&self) -> u32 {
        self.fs_context.umask()
    }
    fn can_access_file(&self, inode: &Inode, req_perms: PermMask) -> bool {
        let creds = &self.credentials;
        let uattr = match inode.unstable_attr() {
            Ok(v) => v,
            Err(_) => return false,
        };

        let perms = if uattr.owner.uid == creds.effective_kuid {
            uattr.perms.user
        } else if creds.in_group(uattr.owner.gid) {
            uattr.perms.group
        } else {
            uattr.perms.other
        };

        let stable_attr = inode.stable_attr();
        if stable_attr.is_file() && req_perms.execute && inode.mount_source().flags().no_exec {
            return false;
        }
        if perms.is_superset_of(&req_perms) {
            return true;
        }
        if stable_attr.is_directory() {
            if inode.check_capability(&linux::Capability::dac_override(), self) {
                return true;
            }

            if !req_perms.write
                && inode.check_capability(&linux::Capability::dac_read_search(), self)
            {
                return true;
            }
        }

        if (!req_perms.execute || uattr.perms.any_execute())
            && inode.check_capability(&linux::Capability::dac_override(), self)
        {
            return true;
        }

        req_perms.is_read_only()
            && inode.check_capability(&linux::Capability::dac_read_search(), self)
    }
    fn file_owner(&self) -> FileOwner {
        FileOwner {
            uid: self.credentials.effective_kuid,
            gid: self.credentials.effective_kgid,
        }
    }
    fn single_io_sequence(
        &self,
        _addr: Addr,
        _length: i32,
        _opts: IoOpts,
    ) -> SysResult<IoSequence> {
        unimplemented!()
    }
    fn new_fd_from(&self, _fd: i32, _file: &Rc<RefCell<File>>, _flags: FdFlags) -> SysResult<i32> {
        unimplemented!()
    }
}

#[cfg(test)]
pub struct TestMemoryFileProvider {
    memory_file: Rc<RwLock<MemoryFile>>,
}

#[cfg(test)]
impl MemoryFileProvider for TestMemoryFileProvider {
    fn memory_file(&self) -> &Rc<RwLock<MemoryFile>> {
        &self.memory_file
    }
    fn memory_file_read_lock(&self) -> RwLockReadGuard<'_, MemoryFile> {
        self.memory_file.read().unwrap()
    }
    fn memory_file_write_lock(&self) -> RwLockWriteGuard<'_, MemoryFile> {
        self.memory_file.write().unwrap()
    }
}

#[cfg(test)]
impl TestMemoryFileProvider {
    pub fn new() -> Self {
        let memfile_name = "test-context-memory";
        let memfd = utils::mem::create_mem_fd(memfile_name, 0)
            .unwrap_or_else(|e| panic!("error creating application memory file: {:?}", e));
        let memfile = unsafe { StdFile::from_raw_fd(memfd) };
        let memory_file = MemoryFile::new(memfile, MemoryFileOpts::default())
            .expect("error creating pgalloc::MemoryFile");

        Self {
            memory_file: Rc::new(RwLock::new(memory_file)),
        }
    }
}
