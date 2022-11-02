use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    os::unix::prelude::{AsRawFd, RawFd},
    path::PathBuf,
    rc::Rc,
    sync::{RwLock, RwLockReadGuard, RwLockWriteGuard},
};

use anyhow::Context as AnyhowContext;
use oci_spec::runtime::Spec;
use sentinel_oci::{SentinelConfig, SentinelNamespaces};
use smoltcp::{
    iface::{Interface, InterfaceBuilder, NeighborCache, Routes, SocketHandle},
    phy::{self, Medium, TunTapInterface},
    socket::Socket,
    time::{Duration, Instant},
    wire::{EthernetAddress, IpAddress, IpCidr, Ipv4Address, Ipv6Address},
};

use auth::credentials::Credentials;
use fs::{
    attr::{FileOwner, PermMask, StableAttr},
    host,
    inode::Inode,
    mount::{MountNamespace, MountSource, MountSourceFlags},
    setup_fs, Dirent, DirentRef, DockerImageInfo, FsContext,
};
use limit::LimitSet;
use nix::{sys::ptrace, unistd::Pid};
use once_cell::sync::OnceCell;
use platform::Platform;
use time::{Clock, HostClock, Time, Context as TimeContext};
use usage::memory::init_memory_accounting;

use crate::{
    kernel::{task::Task, Kernel},
    mm::MemoryManager,
};

pub struct Context {
    limits: RwLock<LimitSet>,
    credentials: Credentials,
    kernel: Kernel,
    tid: Option<Pid>,
    task: RwLock<Task>,
    fs_context: Option<FsContext>,
    platform: Platform,
    real_time_clock: Option<HostClock>,
    envv: HashMap<String, String>,
    executable_path: PathBuf,
    argv: Vec<String>,
    network_interface: RwLock<Interface<'static, TunTapInterface>>,
    network_device_fd: RawFd,
    used_ports: RwLock<HashSet<u16>>,
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("limits", &self.limits)
            .field("credentials", &self.credentials)
            .field("kernel", &self.kernel)
            .field("tid", &self.tid)
            .field("task", &self.task)
            .field("fs_context", &self.fs_context)
            .field("platform", &self.platform)
            .field("real_time_clock", &self.real_time_clock)
            .field("envv", &self.envv)
            .field("argv", &self.argv)
            .finish()
    }
}

unsafe impl Send for Context {}
unsafe impl Sync for Context {}

static CONTEXT: OnceCell<RwLock<Context>> = OnceCell::new();

pub fn init_context(
    limits: RwLock<LimitSet>,
    credentials: Credentials,
    kernel: Kernel,
    spec: &Spec,
    namespace: &SentinelNamespaces,
    config: &SentinelConfig,
    command: &[String],
) -> anyhow::Result<()> {
    let platform = kernel.platform();

    let mounts = {
        let flags = MountSourceFlags::default();
        let msrc = Rc::new(MountSource::new(flags));
        let stable_attr =
            StableAttr::from_path("/").expect("failed to retrieve StableAttr from fd");
        let dir = host::Dir::new("/", &now);
        let inode = Inode::new(Box::new(dir), msrc, stable_attr);
        let root = Dirent::new(inode, "/".to_string());
        MountNamespace::new(root)
    };

    init_memory_accounting();

    let mut routes = Routes::new(BTreeMap::new());
    let default_v4_gw = Ipv4Address::new(192, 168, 69, 100);
    let default_v6_gw = Ipv6Address::new(0xfe80, 0, 0, 0, 0, 0, 0, 0x100);
    routes.add_default_ipv4_route(default_v4_gw)?;
    routes.add_default_ipv6_route(default_v6_gw)?;

    let ip_addrs = [
        IpCidr::new(IpAddress::v4(192, 168, 69, 1), 24),
        IpCidr::new(IpAddress::v6(0xfdaa, 0, 0, 0, 0, 0, 0, 1), 64),
        IpCidr::new(IpAddress::v6(0xfe80, 0, 0, 0, 0, 0, 0, 1), 64),
    ];
    let ethernet_addr = EthernetAddress([0x02, 0x0, 0x0, 0x0, 0x0, 0x02]);
    let neighbor_cache = NeighborCache::new(BTreeMap::new());
    let dev = TunTapInterface::new("tap100", Medium::Ethernet)
        .expect("failed to initialize TunTapInterface");
    let network_device_fd = dev.as_raw_fd();

    let iface = InterfaceBuilder::new(dev, vec![])
        .ip_addrs(ip_addrs)
        .routes(routes)
        .hardware_addr(ethernet_addr.into())
        .neighbor_cache(neighbor_cache)
        .finalize();
    let network_interface = RwLock::new(iface);

    let task = RwLock::new(Task::new(mounts.clone()).expect("failed to initialize task"));
    let ctx = Context {
        limits,
        credentials,
        kernel,
        tid: None,
        task,
        fs_context: None,
        platform,
        real_time_clock: None,
        envv: HashMap::new(),            // set this field afterward
        argv: Vec::new(),                // set this field afterward
        executable_path: PathBuf::new(), // set this field afterward
        network_interface,
        network_device_fd,
        used_ports: RwLock::new(HashSet::new()),
    };
    CONTEXT
        .set(RwLock::new(ctx))
        .map_err(|_| anyhow::anyhow!("context is already set"))?;

    let docker_image_info = if cfg!(test) {
        DockerImageInfo::default()
    } else {
        config
            .run_create_container_hooks()
            .with_context(|| "CreateContainer hooks")?;
        let ctx = &*context();
        setup_fs(
            spec,
            namespace,
            config.state.container_id().to_string(),
            mounts,
            command,
            ctx,
        )?
    };

    // set the correct values.
    {
        let mut ctx = context_mut();
        let fs_ctx = FsContext::new(docker_image_info.root, docker_image_info.cwd, 0o22);
        ctx.set_fs_context(fs_ctx);
        ctx.envv = docker_image_info.envv;
        ctx.argv = command.iter().map(|s| s.to_string()).collect();
        ctx.executable_path = docker_image_info.executable_path;
    }
    Ok(())
}

pub fn context() -> RwLockReadGuard<'static, Context> {
    CONTEXT
        .get()
        .expect("Context is not set")
        .read()
        .expect("failed to acquire read lock")
}

pub fn context_mut() -> RwLockWriteGuard<'static, Context> {
    CONTEXT
        .get()
        .expect("Context is not set")
        .write()
        .expect("failed to acquire write lock")
}

#[cfg(test)]
pub fn init_for_test() {
    if CONTEXT.get().is_some() {
        return;
    }
    let creds = Credentials::new_anonymous();
    let kernel = Kernel::load();
    init_context(
        RwLock::new(LimitSet::default()),
        creds,
        kernel,
        &Spec::default(),
        &SentinelNamespaces::default(),
        &SentinelConfig::default(),
        &["dummy command".to_string()],
    )
    .expect("failed to initialize the context");
}

pub fn now() -> Time {
    match CONTEXT.get() {
        Some(c) => c.read().unwrap().now(),
        None => HostClock.now(),
    }
}

impl auth::Context for Context {
    fn credentials(&self) -> &Credentials {
        &self.credentials
    }
}

impl mem::Context for Context {
    fn copy_out_bytes(&self, addr: mem::Addr, src: &[u8]) -> utils::SysResult<usize> {
        self.task().copy_out_bytes(addr, src)
    }
    fn copy_in_bytes(&self, addr: mem::Addr, dst: &mut [u8]) -> utils::SysResult<usize> {
        self.task().copy_in_bytes(addr, dst)
    }
}

impl time::Context for Context {
    fn now(&self) -> Time {
        self.real_time_clock().now()
    }
}

impl limit::Context for Context {
    fn limits(&self) -> LimitSet {
        *self.limits.read().unwrap()
    }
}

impl pgalloc::Context for Context {
    fn memory_file_provider(&self) -> &dyn pgalloc::MemoryFileProvider {
        &self.kernel
    }
}

impl net::Context for Context {
    fn add_socket(&self, socket: Socket<'static>) -> SocketHandle {
        let mut iface = self.network_interface.write().unwrap();
        match socket {
            Socket::Raw(s) => iface.add_socket(s),
            Socket::Tcp(s) => iface.add_socket(s),
            Socket::Udp(s) => iface.add_socket(s),
            Socket::Icmp(s) => iface.add_socket(s),
            Socket::Dhcpv4(s) => iface.add_socket(s),
        }
    }

    #[inline]
    fn network_interface_mut(&self) -> RwLockWriteGuard<'_, Interface<'static, TunTapInterface>> {
        self.network_interface.write().unwrap()
    }

    fn gen_local_port(&self) -> u16 {
        // FIXME: Naive implementation.
        loop {
            let local_port = 49152 + rand::random::<u16>() % 16384;
            if !self.used_ports.read().unwrap().contains(&local_port) {
                self.used_ports.write().unwrap().insert(local_port);
                return local_port;
            }
        }
    }

    fn remove_local_port(&self, p: u16) {
        if !self.used_ports.write().unwrap().remove(&p) {
            logger::info!("removing unused port");
        }
    }

    fn poll_wait(&self, once: bool) {
        let mut iface = self.network_interface_mut();
        while !match iface.poll(Instant::now()) {
            Ok(r) => r,
            Err(err) => {
                logger::warn!("poll failed: {:?}", err);
                true
            }
        } {
            if once {
                break;
            }
            phy::wait(self.network_device_fd, iface.poll_delay(Instant::now()))
                .expect("wait failed");
        }
    }

    fn wait(&self, duration: Option<Duration>) {
        phy::wait(self.network_device_fd, duration).expect("wait failed");
    }

    fn as_net_context(&self)-> &dyn net::Context {
        self
    }
}

impl fs::Context for Context {
    fn working_directory(&self) -> &DirentRef {
        self.fs_context
            .as_ref()
            .expect("FsContext is not set")
            .working_directory()
    }
    fn root_directory(&self) -> &DirentRef {
        self.fs_context
            .as_ref()
            .expect("FsContext is not set")
            .root_directory()
    }
    fn umask(&self) -> u32 {
        self.fs_context
            .as_ref()
            .expect("FsContext is not set")
            .umask()
    }
    fn can_access_file(&self, inode: &Inode, req_perms: PermMask) -> bool {
        let creds = &self.credentials;
        let uattr = match inode.unstable_attr() {
            Ok(v) => v,
            Err(_) => return false,
        };

        let p = if uattr.owner.uid == creds.effective_kuid {
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

        if p.is_superset_of(&req_perms) {
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
        addr: mem::Addr,
        length: i32,
        opts: mem::IoOpts,
    ) -> utils::SysResult<mem::IoSequence> {
        let task = self.task();
        task.single_io_sequence(addr, length, opts)
    }

    fn new_fd_from(
        &self,
        fd: i32,
        file: &Rc<RefCell<fs::File>>,
        flags: fs::FdFlags,
    ) -> utils::SysResult<i32> {
        let mut task = self.task_mut();
        task.new_fd_from(fd, file, flags)
    }
}

impl platform::Context for Context {
    fn tid(&self) -> Pid {
        self.tid.expect("tid is not loaded yet")
    }
    fn task_init_regs(&self) -> libc::user_regs_struct {
        self.task().init_regs()
    }
    fn ptrace_set_regs(&self, regs: libc::user_regs_struct) -> nix::Result<()> {
        let pid = self.tid();
        ptrace::setregs(pid, regs)
    }
}

impl memmap::Context for Context {
    fn mm(&self) -> Rc<RefCell<dyn memmap::MemoryInvalidator>> {
        self.memory_manager()
    }
}

impl Context {
    #[inline]
    pub fn platform(&self) -> Platform {
        self.platform
    }

    #[inline]
    pub fn limits_mut(&self) -> RwLockWriteGuard<'_, LimitSet> {
        self.limits.write().unwrap()
    }

    #[inline]
    pub fn envv(&self) -> &HashMap<String, String> {
        &self.envv
    }

    #[inline]
    pub fn executable_path(&self) -> &PathBuf {
        &self.executable_path
    }

    #[inline]
    pub fn argv(&self) -> &Vec<String> {
        &self.argv
    }

    #[inline]
    pub fn set_tid(&mut self, pid: Pid) {
        self.tid = Some(pid);
    }

    #[inline]
    fn set_fs_context(&mut self, fs_context: FsContext) {
        self.fs_context = Some(fs_context);
    }

    #[inline]
    pub fn task(&self) -> RwLockReadGuard<'_, Task> {
        self.task
            .try_read()
            .expect("failed to acquire read lock from context.task")
    }

    #[inline]
    pub fn task_mut(&self) -> RwLockWriteGuard<'_, Task> {
        self.task
            .try_write()
            .expect("failed to acquire write lock from context.task")
    }

    #[inline]
    pub fn kernel(&self) -> &Kernel {
        &self.kernel
    }

    #[inline]
    pub fn real_time_clock(&self) -> HostClock {
        self.real_time_clock.unwrap_or(HostClock)
    }

    #[inline]
    pub fn memory_manager(&self) -> Rc<RefCell<MemoryManager>> {
        self.task().memory_manager().clone()
    }

    #[cfg(test)]
    pub fn set_limits(&mut self, limits: LimitSet) {
        *self.limits.write().unwrap() = limits;
    }

    pub fn set_working_directory(&mut self, dir: DirentRef) {
        self.fs_context
            .as_mut()
            .expect("fs_context not set")
            .set_working_directory(dir)
    }
}
