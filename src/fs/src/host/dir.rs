use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    rc::Rc,
};

use mem::PAGE_SIZE;
use memmap::mmap_opts::MmapOpts;
use time::Time;
use usage::MemoryKind;
use utils::{bail_libc, SysError, SysResult};

use crate::{
    attr::{FilePermissions, InodeType, StableAttr, UnstableAttr},
    context::Context,
    dentry::{generic_readdir, DentAttr, DentrySerializer, DirIterCtx},
    dev::null::NullDevice,
    dirent_readdir,
    fsutils::{inode::InodeSimpleAttributes, seek_with_dir_cursor},
    inode,
    inode_operations::RenameUnderParents,
    mount::{MountSource, MountSourceFlags},
    seek::SeekWhence,
    tmpfs::{self, TMPFS_DEVICE},
    DirIterator, Dirent, DirentRef, File, FileFlags, FileOperations, InodeOperations, ReaddirError,
    ReaddirResult,
};

use super::{symlink::Symlink, RegularFile};

#[derive(Debug)]
struct DirChildren {
    // already_read indicates whether this Dir is already read through readdir or not.
    // Initially this field is false, and set to true once read.
    already_read: bool,
    dirents: HashMap<String, DirentRef>,
    dentry_map: BTreeMap<String, DentAttr>,
}

impl DirChildren {
    fn new() -> Self {
        Self {
            already_read: false,
            dirents: HashMap::new(),
            dentry_map: BTreeMap::new(),
        }
    }

    fn dirents(&mut self, dir_path: &PathBuf, ctx: &dyn Context) -> &HashMap<String, DirentRef> {
        if !self.already_read {
            self.retrieve_children(dir_path, ctx);
            self.already_read = true;
        }
        &self.dirents
    }

    fn dentry_map(&mut self, dir_path: &PathBuf, ctx: &dyn Context) -> &BTreeMap<String, DentAttr> {
        if !self.already_read {
            self.retrieve_children(dir_path, ctx);
            self.already_read = true;
        }
        &self.dentry_map
    }

    fn retrieve_children(&mut self, dir_path: &PathBuf, ctx: &dyn Context) {
        let mut dir = nix::dir::Dir::open(
            dir_path,
            nix::fcntl::OFlag::O_RDONLY | nix::fcntl::OFlag::O_DIRECTORY,
            nix::sys::stat::Mode::S_IRUSR,
        )
        .expect("failed to open directory");
        for d in dir.iter() {
            let d = d.unwrap();
            let name = d.file_name().to_str().unwrap().to_string();
            if &name == "." || &name == ".." {
                continue;
            }
            let joined = Path::new(dir_path).join(&name);
            let sattr = match StableAttr::from_path(&joined) {
                Ok(a) => a,
                Err(err) => {
                    logger::warn!("failed to retrieve StableAttr from {:?}: {:?}", joined, err);
                    return;
                }
            };
            let iops = dir_or_file(sattr, joined, ctx);
            let msrc = {
                let flags = MountSourceFlags::default();
                MountSource::new(flags)
            };
            let inode = inode::Inode::new(iops, Rc::new(msrc), sattr);
            let d = Dirent::new(inode, name.to_string());
            self.dirents.insert(name.clone(), d);

            let dattr = DentAttr {
                typ: sattr.typ,
                inode_id: sattr.inode_id,
            };
            self.dentry_map.insert(name, dattr);
        }
    }
}

#[derive(Debug)]
pub struct Dir {
    attr: InodeSimpleAttributes,
    children: DirChildren,
    host_absolute_path: PathBuf,
}

impl InodeOperations for Dir {
    fn lookup(&mut self, name: &str, ctx: &dyn Context) -> SysResult<DirentRef> {
        if name.len() > libc::FILENAME_MAX as usize {
            bail_libc!(libc::ENAMETOOLONG);
        }
        self.walk(name, ctx)
    }
    fn get_file(&self, dirent: DirentRef, mut flags: FileFlags) -> SysResult<File> {
        flags.pread = true;
        Ok(File::new(
            flags,
            Box::new(DirFileOperations {
                dirent,
                dir_cursor: String::new(),
            }),
        ))
    }
    fn unstable_attr(&self, msrc: &Rc<MountSource>, sattr: StableAttr) -> SysResult<UnstableAttr> {
        self.attr.unstable_attr(msrc, sattr)
    }
    fn get_link(&self) -> SysResult<DirentRef> {
        bail_libc!(libc::ENOLINK)
    }
    fn read_link(&self) -> SysResult<String> {
        bail_libc!(libc::ENOLINK)
    }
    fn truncate(&mut self, _: i64, _: &dyn Context) -> SysResult<()> {
        bail_libc!(libc::EISDIR)
    }
    fn create(
        &mut self,
        parent_uattr: UnstableAttr,
        mount_source: Rc<MountSource>,
        name: &str,
        flags: FileFlags,
        perms: FilePermissions,
        ctx: &dyn Context,
    ) -> SysResult<File> {
        if name.len() > linux::NAME_MAX {
            bail_libc!(libc::ENAMETOOLONG);
        }
        let inode = self.new_file(parent_uattr, mount_source, perms, ctx)?;
        let dirent = Dirent::new(inode, name.to_string());
        self.add_child(name.to_string(), Rc::clone(&dirent), ctx);
        let dirent_to_add = Rc::clone(&dirent);
        let dirent = dirent.borrow();
        dirent.inode().get_file(dirent_to_add, flags)
    }
    fn rename(
        &self,
        _: RenameUnderParents<&mut inode::Inode>,
        _: &str,
        _: String,
        _: bool,
        _: &dyn Context,
    ) -> SysResult<()> {
        logger::warn!("renaming is only allowed for the files that were created by user");
        bail_libc!(libc::EPERM)
    }
    fn add_link(&self) {
        self.attr.add_link();
    }
    fn drop_link(&self) {
        self.attr.drop_link();
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl Dir {
    pub fn new<P: AsRef<Path>, F: Fn() -> Time>(path: P, timer: F) -> Self {
        let uattr = UnstableAttr::from_path(path.as_ref())
            .expect("failed to retrieve UnstableAttr from path")
            .record_current_time(timer);
        Self {
            attr: InodeSimpleAttributes::new_with_unstable(uattr, linux::RAMFS_MAGIC),
            children: DirChildren::new(),
            host_absolute_path: path.as_ref().to_path_buf(),
        }
    }

    fn walk(&mut self, name: &str, ctx: &dyn Context) -> SysResult<DirentRef> {
        let children_dirents = self.children.dirents(&self.host_absolute_path, ctx);
        children_dirents
            .get(name)
            .cloned()
            .ok_or_else(|| SysError::new(libc::ENOENT))
    }

    fn new_file(
        &self,
        parent_uattr: UnstableAttr,
        dir_mount_source: Rc<MountSource>,
        perms: FilePermissions,
        ctx: &dyn Context,
    ) -> SysResult<inode::Inode> {
        let mut owner = ctx.file_owner();
        if parent_uattr.perms.set_gid {
            owner.gid = parent_uattr.owner.gid;
        }
        let uattr = UnstableAttr {
            owner,
            perms,
            ..UnstableAttr::default()
        };
        let uattr = uattr.record_current_time(|| ctx.now());
        let iops = tmpfs::RegularFile::new_file_in_memory(MemoryKind::Tmpfs, uattr);
        let tmpfs_dev = TMPFS_DEVICE.lock().unwrap();
        Ok(inode::Inode::new(
            Box::new(iops),
            dir_mount_source,
            StableAttr {
                typ: InodeType::RegularFile,
                device_id: tmpfs_dev.device_id(),
                inode_id: tmpfs_dev.next_ino(),
                block_size: PAGE_SIZE as i64,
                device_file_major: 0,
                device_file_minor: 0,
            },
        ))
    }

    fn add_child(&mut self, name: String, d: DirentRef, ctx: &dyn Context) {
        let d_ref = d.borrow();
        let inode = d_ref.inode();
        let sattr = inode.stable_attr();
        self.children.dirents.insert(name.clone(), Rc::clone(&d));
        self.children.dentry_map.insert(
            name,
            DentAttr {
                typ: sattr.typ,
                inode_id: sattr.inode_id,
            },
        );

        if sattr.is_directory() {
            self.attr.add_link();
        }

        inode.add_link();
        let now = ctx.now();
        self.attr.uattr.write().unwrap().modification_time = now;
        self.attr.uattr.write().unwrap().status_change_time = now;
    }

    fn remove_child(&mut self, name: &str, ctx: &dyn Context) -> SysResult<DirentRef> {
        let dirent = self
            .children
            .dirents
            .remove(name)
            .ok_or_else(|| SysError::new(libc::EACCES))?;
        self.children
            .dentry_map
            .remove(name)
            .expect("child existed in dirents but not in dentry_map?");

        let now = ctx.now();
        let mut uattr = self.attr.uattr.write().unwrap();
        uattr.modification_time = now;

        {
            let d_ref = dirent.borrow();
            let inode = d_ref.inode();

            if inode.stable_attr().is_directory() {
                self.drop_link();
            }
            inode.drop_link();
        }

        let now = ctx.now();
        uattr.modification_time = now;
        uattr.status_change_time = now;
        Ok(dirent)
    }
}

fn dir_or_file(
    sattr: StableAttr,
    absolute_path: PathBuf,
    ctx: &dyn Context,
) -> Box<dyn InodeOperations> {
    match sattr.typ {
        InodeType::RegularFile | InodeType::SpecialFile => {
            Box::new(RegularFile::new(absolute_path))
        }
        InodeType::Directory | InodeType::SpecialDirectory => {
            Box::new(Dir::new(absolute_path, &|| ctx.now()))
        }
        InodeType::Symlink => {
            let file_owner = ctx.file_owner();
            let perms = FilePermissions::from_mode(linux::FileMode(0o777));
            let simple_attr =
                InodeSimpleAttributes::new(file_owner, perms, linux::RAMFS_MAGIC, &|| ctx.now());
            let target = PathBuf::from(
                nix::fcntl::readlink(&absolute_path).expect("failed to read symlink"),
            );
            Box::new(Symlink::new(simple_attr, target))
        }
        InodeType::CharacterDevice => {
            // FIXME
            if absolute_path == Path::new("/dev/null") {
                let file_owner = ctx.file_owner();
                let mode = linux::FileMode(0o666);
                Box::new(NullDevice::new(file_owner, mode, ctx))
            } else {
                logger::warn!(
                    "unhandled filename {:?}. Just handling this just like /dev/null",
                    absolute_path
                );
                let file_owner = ctx.file_owner();
                let mode = linux::FileMode(0o666);
                Box::new(NullDevice::new(file_owner, mode, ctx))
            }
        }
        InodeType::BlockDevice => {
            // FIXME
            logger::warn!(
                "unhandled filename {:?}. Just handling this just like /dev/null",
                absolute_path
            );
            let file_owner = ctx.file_owner();
            let mode = linux::FileMode(0o666);
            Box::new(NullDevice::new(file_owner, mode, ctx))
        }
        _ => todo!("unhandled case: {:?}", sattr.typ),
    }
}

#[derive(Debug)]
pub struct DirFileOperations {
    dirent: DirentRef,
    dir_cursor: String,
}

impl FileOperations for DirFileOperations {
    fn dirent(&self) -> DirentRef {
        self.dirent.clone()
    }
    fn read(
        &self,
        _: FileFlags,
        _: &mut mem::IoSequence,
        _: i64,
        _: &dyn Context,
    ) -> SysResult<usize> {
        bail_libc!(libc::EISDIR)
    }
    fn write(
        &self,
        _: FileFlags,
        _: &mut mem::IoSequence,
        _: i64,
        _: &dyn Context,
    ) -> SysResult<usize> {
        bail_libc!(libc::EISDIR)
    }
    fn configure_mmap(&mut self, _: &mut MmapOpts) -> SysResult<()> {
        bail_libc!(libc::ENODEV)
    }
    fn flush(&self) -> SysResult<()> {
        Ok(())
    }
    fn close(&self) -> SysResult<()> {
        Ok(())
    }
    fn ioctl(&self, _: &libc::user_regs_struct, _: &dyn Context) -> SysResult<usize> {
        bail_libc!(libc::ENOTTY)
    }
    fn seek(
        &mut self,
        inode: &inode::Inode,
        whence: SeekWhence,
        current_offset: i64,
        offset: i64,
    ) -> SysResult<i64> {
        seek_with_dir_cursor(
            inode,
            whence,
            current_offset,
            offset,
            Some(&mut self.dir_cursor),
        )
    }
    fn readdir(
        &mut self,
        offset: i64,
        serializer: &mut dyn DentrySerializer,
        ctx: &dyn Context,
    ) -> ReaddirResult<i64> {
        let root = ctx.root_directory();
        let dirent = self.dirent.clone();
        let mut dir_ctx = DirIterCtx {
            serializer,
            attrs: HashMap::new(),
            dir_cursor: Some(&mut self.dir_cursor),
        };
        let it = DirFileIter;
        dirent_readdir(&dirent, &it, root, offset, &mut dir_ctx, ctx)
    }
    fn readiness(&self, mask: u64, _: &dyn Context) -> u64 {
        mask
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

struct DirFileIter;

impl DirIterator for DirFileIter {
    fn iterate_dir(
        &self,
        inode: &mut inode::Inode,
        dir_ctx: &mut DirIterCtx,
        offset: i32,
        ctx: &dyn Context,
    ) -> ReaddirResult<i32> {
        let dir = inode.inode_operations_mut::<Dir>();
        match generic_readdir(
            dir_ctx,
            dir.children.dentry_map(&dir.host_absolute_path, ctx),
        ) {
            Ok(n) => Ok(offset + n),
            Err(err) => Err(ReaddirError::new(offset + err.value(), err.code())),
        }
    }
}

pub fn rename(
    parents: RenameUnderParents<&mut Dir>,
    old_name: &str,
    new_name: String,
    is_replacement: bool,
    ctx: &dyn Context,
) -> SysResult<()> {
    if new_name.len() > linux::NAME_MAX {
        bail_libc!(libc::ENAMETOOLONG);
    }
    match parents {
        RenameUnderParents::Same(parent) => {
            if is_replacement {
                let replaced = parent
                    .children
                    .dirents
                    .get(&new_name)
                    .expect("no child while this rename operation is a replacement");
                let replaced = replaced.borrow();
                if replaced.inode().stable_attr().is_directory() {
                    todo!()
                }
                drop(replaced);
                parent.remove_child(&new_name, ctx)?;
            }

            let d = parent.remove_child(old_name, ctx)?;
            parent.add_child(new_name, d, ctx);
            Ok(())
        }
        RenameUnderParents::Different { old, new } => {
            if is_replacement {
                let replaced = new
                    .children
                    .dirents
                    .get(&new_name)
                    .expect("no child while this rename operation is a replacement");
                let replaced = replaced.borrow();
                if replaced.inode().stable_attr().is_directory() {
                    todo!()
                }
                drop(replaced);
                new.remove_child(&new_name, ctx)?;
            }

            let d = old.remove_child(old_name, ctx)?;
            new.add_child(new_name, d, ctx);
            Ok(())
        }
    }
}
