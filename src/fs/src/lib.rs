pub mod attr;
mod context;
pub mod dentry;
pub mod dev;
mod dirent;
mod fd_flags;
mod file;
mod file_operations;
pub mod fsutils;
pub mod host;
pub mod inode;
mod inode_operations;
pub mod mount;
pub mod offset;
pub mod seek;
pub mod socket;
pub mod tmpfs;
pub mod utils;

pub mod file_test_utils;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context as AnyhowCtx;
use attr::PermMask;
pub use context::*;
pub use dirent::*;
pub use fd_flags::FdFlags;
pub use file::*;
pub use file_operations::FileOperations;
pub use inode_operations::*;

use ::utils::{bail_libc, SysError, SysResult};
use libcontainer::rootfs::RootFS;
use nix::mount::{MntFlags, MsFlags};
use oci_spec::runtime::{LinuxNamespaceType, Spec};
use sentinel_oci::SentinelNamespaces;

use self::mount::MountNamespace;

#[derive(Debug, Default)]
pub struct DockerImageInfo {
    // pub driver_data: HashMap<String, String>,
    pub envv: HashMap<String, String>,
    pub root: Option<DirentRef>,
    pub cwd: Option<DirentRef>,
    pub executable_path: PathBuf, // basically argv[0], but in resolved style.
}

pub fn setup_fs(
    spec: &Spec,
    namespace: &SentinelNamespaces,
    hostname: String,
    mounts: MountNamespace,
    command: &[String],
    ctx: &dyn Context,
) -> anyhow::Result<DockerImageInfo> {
    logger::info!("setting up rootfs");
    let rootfs = RootFS::new();
    let root_path = {
        let root = spec.root().as_ref().context("no root?")?;
        root.path()
            .canonicalize()
            .context("failed to canonicalize")?
    };

    rootfs
        .prepare_rootfs(
            spec,
            &root_path,
            namespace.get(LinuxNamespaceType::User).is_some(),
            namespace.get(LinuxNamespaceType::Cgroup).is_some(),
        )
        .with_context(|| "failed to prepare rootfs")?;
    pivot_root(&root_path).with_context(|| "failed to pivot_root")?;
    logger::info!("rootfs setup done");

    let linux = spec.linux().as_ref().with_context(|| "no linux in spec")?;
    rootfs
        .adjust_root_mount_propagation(linux)
        .with_context(|| "failed to set propagation type of root mount")?;

    remount_read_only()?;

    let root = {
        let mut remaining_traversals = linux::MAX_SYMLINK_TRAVERSALS as u32;
        mounts
            .find_inode(mounts.root(), None, "/", &mut remaining_traversals, ctx)
            .expect("failed to traverse container root")
    };

    let process = spec.process().as_ref().unwrap();
    let wd = process.cwd();

    let cwd = {
        let mut remaining_traversals = linux::MAX_SYMLINK_TRAVERSALS as u32;
        mounts
            .find_inode(mounts.root(), None, &wd, &mut remaining_traversals, ctx)
            .expect("failed to traverse container root")
    };

    let envv = construct_env(process.env().as_ref().unwrap(), hostname);
    let executable_path = resolve_executable_path(
        &command.get(0).expect("no command provided"),
        &mounts,
        &root,
        wd,
        envv.get("PATH").expect("$PATH not found"),
        ctx,
    )?;
    logger::info!("executable path is {:?}", executable_path);

    Ok(DockerImageInfo {
        envv,
        root: Some(root),
        cwd: Some(cwd),
        executable_path,
    })
}

fn pivot_root<P: AsRef<Path>>(root_path: P) -> anyhow::Result<()> {
    std::env::set_current_dir(root_path).with_context(|| "failed to set current directory")?;
    nix::unistd::pivot_root(".", ".").with_context(|| "failed to pivot_root")?;
    nix::mount::umount2(".", MntFlags::MNT_DETACH).with_context(|| "failed to umount2")?;
    Ok(())
}

fn remount_read_only() -> anyhow::Result<()> {
    nix::mount::mount(
        None::<&str>,
        "/",
        None::<&str>,
        MsFlags::MS_RDONLY | MsFlags::MS_REMOUNT | MsFlags::MS_BIND,
        None::<&str>,
    )
    .with_context(|| "failed to mount read only")
}

fn construct_env(env: &Vec<String>, hostname: String) -> HashMap<String, String> {
    let mut envv = HashMap::new();
    for e in env {
        let kv = e.split('=').collect::<Vec<_>>();
        envv.insert(kv[0].to_string(), kv[1].to_string());
    }
    // FIXME: set proper $HOME variable
    envv.insert("HOME".to_string(), "/root".to_string());
    envv.insert("HOSTNAME".to_string(), hostname);
    envv
}

fn resolve_executable_path<P: AsRef<Path>, D: AsRef<Path>>(
    exec: P,
    mounts: &MountNamespace,
    root: &DirentRef,
    working_directory: D,
    path_env: &str,
    ctx: &dyn Context,
) -> SysResult<PathBuf> {
    if exec.as_ref().is_absolute() {
        let mut buf = PathBuf::new();
        buf.push(exec);
        return Ok(buf);
    }
    if exec.as_ref().components().count() > 1 {
        return Ok(working_directory.as_ref().join(exec));
    }

    for p in path_env.split(':') {
        let candidate = Path::new(p).join(exec.as_ref());
        let mut remaining_traversals = linux::MAX_SYMLINK_TRAVERSALS as u32;
        match mounts.find_inode(root, None, &candidate, &mut remaining_traversals, ctx) {
            Ok(ref d) => {
                let d_ref = d.borrow();
                let inode = d_ref.inode();
                if !inode.stable_attr().is_regular() {
                    continue;
                }
                if inode
                    .check_permission(
                        PermMask {
                            read: true,
                            write: false,
                            execute: true,
                        },
                        ctx,
                    )
                    .is_err()
                {
                    logger::info!(
                        "Found executable at {:?} but the user lacks permission to execute it",
                        candidate
                    );
                    continue;
                }
                return Ok(Path::new("/").join(p).join(exec));
            }
            Err(err) if err.code() == libc::ENOENT || err.code() == libc::EACCES => continue,
            Err(err) => return Err(err),
        }
    }
    bail_libc!(libc::ENOENT)
}
