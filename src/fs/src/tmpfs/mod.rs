use std::{
    rc::Rc,
    sync::{Arc, Mutex},
};

use once_cell::sync::Lazy;

use dev::Device;

mod regular;

pub use regular::*;
use utils::{bail_libc, SysError, SysResult};

use crate::{host, inode::Inode, inode_operations::RenameUnderParents, Context};

pub static TMPFS_DEVICE: Lazy<Arc<Mutex<Device>>> = Lazy::new(Device::new_anonymous_device);

pub fn rename(
    parents: RenameUnderParents<&mut Inode>,
    old_name: &str,
    new_name: String,
    is_replacement: bool,
    ctx: &dyn Context,
) -> SysResult<()> {
    match parents {
        RenameUnderParents::Same(parent) => {
            let parent = parent.inode_operations_mut::<host::Dir>();
            host::rename(
                RenameUnderParents::Same(parent),
                old_name,
                new_name,
                is_replacement,
                ctx,
            )
        }
        RenameUnderParents::Different { old, new } => {
            if Rc::as_ptr(old.mount_source()) != Rc::as_ptr(new.mount_source()) {
                bail_libc!(libc::EXDEV);
            }
            let old = old.inode_operations_mut::<host::Dir>();
            let new = new.inode_operations_mut::<host::Dir>();
            host::rename(
                RenameUnderParents::Different { old, new },
                old_name,
                new_name,
                is_replacement,
                ctx,
            )
        }
    }
}
