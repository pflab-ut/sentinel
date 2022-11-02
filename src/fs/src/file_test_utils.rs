use crate::{File, FileFlags};

use super::{context::Context, dirent::Dirent, inode::Inode, tmpfs};

pub fn new_test_file(ctx: &dyn Context) -> File {
    let dirent = Dirent::new(Inode::new_anon(&|| ctx.now()), "test".to_string());
    File::new(
        FileFlags::default(),
        Box::new(tmpfs::RegularFileOperations { dirent }),
    )
}
