use std::{cell::RefCell, collections::VecDeque, hash::Hash, rc::Rc, sync::RwLock};

use fs::{inode::Inode, Dirent, DirentRef, FileFlags, FileOperations, ReaddirError};
use time::Context;
use utils::{bail_libc, SysError, SysResult};

use crate::context;

#[derive(Debug)]
struct FileIdentifier {
    file: Rc<RefCell<fs::File>>,
    fd: i32,
}

impl Hash for FileIdentifier {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        Rc::as_ptr(&self.file).hash(state);
        self.fd.hash(state);
    }
}

impl PartialEq for FileIdentifier {
    fn eq(&self, other: &Self) -> bool {
        Rc::as_ptr(&self.file) == Rc::as_ptr(&other.file) && self.fd == other.fd
    }
}

impl Eq for FileIdentifier {}

#[derive(Debug, Clone)]
struct PollEntry {
    file: Rc<RefCell<fs::File>>,
    mask: u64,
}

impl Hash for PollEntry {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        Rc::as_ptr(&self.file).hash(state);
        self.mask.hash(state);
    }
}

impl PartialEq for PollEntry {
    fn eq(&self, other: &Self) -> bool {
        Rc::as_ptr(&self.file) == Rc::as_ptr(&other.file) && self.mask == other.mask
    }
}

impl Eq for PollEntry {}

#[derive(Debug)]
pub struct EventPoll {
    dirent: DirentRef,
    ready_queue: RwLock<VecDeque<PollEntry>>,
    waiting_queue: RwLock<VecDeque<PollEntry>>,
}

impl FileOperations for EventPoll {
    fn dirent(&self) -> fs::DirentRef {
        self.dirent.clone()
    }
    fn read(
        &self,
        _: fs::FileFlags,
        _: &mut mem::IoSequence,
        _: i64,
        _: &dyn fs::Context,
    ) -> SysResult<usize> {
        bail_libc!(libc::ENOSYS)
    }
    fn write(
        &self,
        _: fs::FileFlags,
        _: &mut mem::IoSequence,
        _: i64,
        _: &dyn fs::Context,
    ) -> SysResult<usize> {
        bail_libc!(libc::ENOSYS)
    }
    fn configure_mmap(&mut self, _: &mut memmap::mmap_opts::MmapOpts) -> SysResult<()> {
        bail_libc!(libc::ENODEV)
    }
    fn flush(&self) -> SysResult<()> {
        Ok(())
    }
    fn close(&self) -> SysResult<()> {
        Ok(())
    }
    fn ioctl(&self, _: &libc::user_regs_struct, _: &dyn fs::Context) -> SysResult<usize> {
        bail_libc!(libc::ENOTTY)
    }
    fn seek(
        &mut self,
        _: &fs::inode::Inode,
        _: fs::seek::SeekWhence,
        _: i64,
        _: i64,
    ) -> SysResult<i64> {
        bail_libc!(libc::ESPIPE)
    }
    fn readdir(
        &mut self,
        _: i64,
        _: &mut dyn fs::dentry::DentrySerializer,
        _: &dyn fs::Context,
    ) -> fs::ReaddirResult<i64> {
        Err(ReaddirError::new(0, libc::ENOTDIR))
    }
    fn readiness(&self, mask: u64, _: &dyn fs::Context) -> u64 {
        if mask & linux::POLL_READABLE_EVENTS != 0 && self.events_available() {
            linux::POLL_READABLE_EVENTS
        } else {
            0
        }
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl EventPoll {
    fn events_available(&self) -> bool {
        let q: VecDeque<PollEntry> = self.ready_queue.read().unwrap().clone();
        let mut ready_queue = self.ready_queue.write().unwrap();
        let mut waiting_queue = self.waiting_queue.write().unwrap();
        for (i, e) in q.iter().enumerate() {
            let f = e.file.borrow();
            let ctx = &*context::context();
            let ready = f.readiness(e.mask, ctx);
            if ready != 0 {
                return true;
            }
            ready_queue.remove(i);
            waiting_queue.push_back(e.clone());
        }
        false
    }
}

pub fn new_event_poll() -> fs::File {
    let ctx = context::context();
    let inode = Inode::new_anon(&|| ctx.now());
    let dirent = Dirent::new(inode, "anon_inode:[eventpoll]".to_string());
    fs::File::new(
        FileFlags::default(),
        Box::new(EventPoll {
            dirent,
            ready_queue: RwLock::new(VecDeque::new()),
            waiting_queue: RwLock::new(VecDeque::new()),
        }),
    )
}
