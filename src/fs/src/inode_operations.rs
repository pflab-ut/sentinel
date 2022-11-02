use std::{any::Any, rc::Rc};

use utils::SysResult;

use crate::{
    attr::{FilePermissions, StableAttr, UnstableAttr},
    inode::Inode,
    mount::MountSource,
    DirentRef, File, FileFlags,
};

use super::Context;

pub enum RenameUnderParents<T> {
    Different { old: T, new: T },
    Same(T),
}

pub trait InodeOperations: std::fmt::Debug {
    fn lookup(&mut self, name: &str, ctx: &dyn Context) -> SysResult<DirentRef>;
    fn get_file(&self, dir: DirentRef, flags: FileFlags) -> SysResult<File>;
    fn unstable_attr(&self, msrc: &Rc<MountSource>, sattr: StableAttr) -> SysResult<UnstableAttr>;
    fn get_link(&self) -> SysResult<DirentRef>;
    fn read_link(&self) -> SysResult<String>;
    fn truncate(&mut self, size: i64, ctx: &dyn Context) -> SysResult<()>;
    fn create(
        &mut self,
        parent_uattr: UnstableAttr,
        mount_source: Rc<MountSource>,
        name: &str,
        flags: FileFlags,
        perms: FilePermissions,
        ctx: &dyn Context,
    ) -> SysResult<File>;
    fn rename(
        &self,
        parents: RenameUnderParents<&mut Inode>,
        old_name: &str,
        new_name: String,
        is_replacement: bool,
        ctx: &dyn Context,
    ) -> SysResult<()>;
    fn add_link(&self);
    fn drop_link(&self);

    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}
