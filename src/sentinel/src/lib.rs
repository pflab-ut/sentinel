mod capabilities;
mod context;
mod kernel;
mod loader;
mod mm;
pub mod oci;
mod syscalls;

use std::{
    collections::{BTreeMap, HashMap},
    ffi::CString,
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    rc::Rc,
    sync::RwLock,
    time::Duration,
};

use anyhow::{anyhow, bail, Context as AnyhowCtx};
use auth::{
    credentials::Credentials,
    id::{Kgid, Kuid},
    user_namespace::UserNamespace,
};
use capabilities::{drop_privileges, set_capabilities};
use kernel::Kernel;
use limit::LimitSet;
use mem::{AccessType, Addr};
use nix::{
    sys::{
        ptrace,
        signal::Signal,
        stat::Mode,
        wait::{waitpid, WaitPidFlag, WaitStatus},
    },
    unistd::{self, Gid, Pid, Uid},
};
use oci_spec::runtime::{LinuxNamespaceType, Spec};
use platform::{stub_init, Context, STUB_START};
use seccompiler::deserialize_binary;
use sentinel_oci::{ContainerStatus, SentinelConfig, SentinelNamespaces};
use utils::SysError;

// This byte limit is passed to `bincode` to guard against a potential memory
// allocation DOS caused by binary filters that are too large.
// This limit can be safely determined since the maximum length of a BPF
// filter is 4096 instructions and Firecracker has a finite number of threads.
const DESERIALIZATION_BYTES_LIMIT: Option<u64> = Some(100_000);

#[derive(Debug)]
pub struct NotifyListener {
    socket: UnixListener,
}

impl NotifyListener {
    pub fn new<P: AsRef<Path>>(socket_path: P) -> anyhow::Result<Self> {
        if !socket_path.as_ref().is_absolute() {
            bail!("socket path {:?} is not absolute", socket_path.as_ref());
        }
        let cwd = std::env::current_dir().context("failed to retrieve cwd")?;
        let parent = socket_path
            .as_ref()
            .parent()
            .context("no parent directory?")?;
        std::env::set_current_dir(parent).context("failed to set cwd")?;
        let socket_name = socket_path
            .as_ref()
            .file_name()
            .context("failed to retrieve file name of socket")?;
        let socket = UnixListener::bind(socket_name)
            .with_context(|| format!("failed to bind {:?}", socket_path.as_ref()))?;
        std::env::set_current_dir(cwd).context("failed to set working directory back")?;
        Ok(Self { socket })
    }

    fn wait(&self) -> anyhow::Result<()> {
        match self.socket.accept() {
            Ok((mut socket, _)) => {
                let mut response = String::new();
                socket
                    .read_to_string(&mut response)
                    .context("failed to read content of socket into string")?;
                logger::debug!("NotifyListener received {}", response);
                Ok(())
            }
            Err(err) => bail!("NotifyListener accept failed: {}", err),
        }
    }

    fn wait_for_pid(&self) -> anyhow::Result<i32> {
        match self.socket.accept() {
            Ok((mut socket, _)) => {
                let mut pid = vec![0; 4];
                socket
                    .read(&mut pid)
                    .context("failed to read content of socket into string")?;
                let pid = i32::from_le_bytes([pid[0], pid[1], pid[2], pid[3]]);
                logger::debug!("NotifyListener received pid {}", pid);
                Ok(pid)
            }
            Err(err) => bail!("NotifyListener accept failed: {}", err),
        }
    }
}

#[derive(Debug)]
pub struct NotifySender {
    path: PathBuf,
}

impl NotifySender {
    pub fn new<P: Into<PathBuf>>(path: P) -> Self {
        Self { path: path.into() }
    }

    pub fn notify(&self, b: &[u8]) -> anyhow::Result<()> {
        let cwd = std::env::current_dir().context("failed to retrieve cwd")?;
        let parent = self.path.parent().context("no parent directory?")?;
        std::env::set_current_dir(parent)
            .with_context(|| format!("failed to set cwd to {:?}", parent))?;
        let mut stream = UnixStream::connect(&self.path.file_name().unwrap())?;
        stream.write_all(b)?;
        std::env::set_current_dir(cwd)?;
        Ok(())
    }

    fn send_pid(&self, pid: i32) -> anyhow::Result<()> {
        let cwd = std::env::current_dir().context("failed to retrieve cwd")?;
        let parent = self.path.parent().context("no parent directory?")?;
        std::env::set_current_dir(parent).context("failed to set cwd")?;
        let mut stream = UnixStream::connect(&self.path.file_name().unwrap())?;
        stream.write_all(&pid.to_le_bytes())?;
        logger::debug!("pid notify done");
        std::env::set_current_dir(cwd)?;
        Ok(())
    }
}

pub fn spawn_sandbox(
    listener: &NotifyListener,
    namespace_setup_notifier: &NotifySender,
    end_notifier: &NotifySender,
    dir: &str,
    spec: &Spec,
    config: &mut SentinelConfig,
) -> anyhow::Result<i32> {
    let sock_path = format!("{}/notidy-pid.sock", dir);
    let pid_listener =
        NotifyListener::new(&sock_path).with_context(|| "failed to create NotifyListener")?;
    let pid_sender = NotifySender::new(sock_path);
    match unsafe { libc::fork() } {
        0 => {
            create_sandbox(
                listener,
                namespace_setup_notifier,
                &pid_sender,
                end_notifier,
                dir,
                spec,
                config,
            )
            .with_context(|| "create sandbox failed")?;
            std::process::exit(0);
        }
        pid if pid > 0 => {
            let pid = pid_listener
                .wait_for_pid()
                .with_context(|| "failed to wait for pid")?;
            logger::debug!("sandbox pid is {}", pid);
            Ok(pid)
        }
        _ => anyhow::bail!("failed to fork"),
    }
}

fn get_cgroup_path(cgroups_path: &Option<PathBuf>, container_id: &str) -> PathBuf {
    match cgroups_path {
        Some(cpath) => cpath.clone(),
        None => PathBuf::from(container_id),
    }
}

fn create_sandbox(
    listener: &NotifyListener,
    namespace_setup_notifier: &NotifySender,
    pid_sender: &NotifySender,
    end_notifier: &NotifySender,
    dir: &str,
    spec: &Spec,
    config: &mut SentinelConfig,
) -> anyhow::Result<()> {
    let linux = spec.linux().as_ref().with_context(|| "no linux found")?;
    let cgroup_path = get_cgroup_path(linux.cgroups_path(), config.state.container_id());
    let cgroup_manager =
        libcgroups::common::create_cgroup_manager(&cgroup_path, false, config.state.container_id())
            .with_context(|| "failed to create cgroup manager")?;
    cgroup_manager
        .add_task(unistd::getppid())
        .with_context(|| "failed to add task to cgroup_manager")?;

    if let Some(resources) = linux.resources().as_ref() {
        let controller_opt = libcgroups::common::ControllerOpt {
            resources,
            freezer_state: None,
            oom_score_adj: None,
            disable_oom_killer: false,
        };
        cgroup_manager
            .apply(&controller_opt)
            .with_context(|| "failed to apply resource limits to cgroup")?;
    }

    let namespaces = SentinelNamespaces::from(linux.namespaces().as_ref());
    if let Some(user_namespace) = namespaces.get(LinuxNamespaceType::User) {
        namespaces
            .unshare_or_setns(user_namespace)
            .with_context(|| "failed to enter user namespace")?;

        if let Err(e) = prctl::set_keep_capabilities(true) {
            anyhow::bail!("set keep capabilities failed with {}", e);
        }
        let gid = Gid::from_raw(0);
        let uid = Uid::from_raw(0);
        unistd::setresgid(gid, gid, gid).with_context(|| "failed to set gid")?;
        unistd::setresuid(uid, uid, uid).with_context(|| "failed to set uid")?;
        if let Err(e) = prctl::set_keep_capabilities(false) {
            anyhow::bail!("set keep capabilities failed with {}", e);
        }
    }

    let proc = spec.process().as_ref().context("no process in spec?")?;
    if let Some(rlimits) = proc.rlimits() {
        for rlimit in rlimits {
            let rlim = &libc::rlimit {
                rlim_cur: rlimit.soft(),
                rlim_max: rlimit.hard(),
            };
            let res = unsafe { libc::setrlimit(rlimit.typ() as u32, rlim) };
            if res < 0 {
                unsafe {
                    let msg = CString::new("failed to set rlimits").unwrap();
                    libc::perror(msg.as_ptr());
                    anyhow::bail!("Failed to set rlimits");
                }
            }
        }
    }

    if let Some(pid_ns) = namespaces.get(LinuxNamespaceType::Pid) {
        namespaces
            .unshare_or_setns(pid_ns)
            .with_context(|| "failed to enter user namespace")?;
    }

    match unsafe { libc::fork() } {
        0 => {
            unistd::setsid().with_context(|| "failed to create session")?;
            apply_rest_namespaces(&namespaces, spec)
                .with_context(|| "failed to apply namespaces")?;
            namespace_setup_notifier
                .notify(b"namespace setup is done")
                .with_context(|| "namespace notification failed")?;

            if let Some(umask) = proc.user().umask() {
                let mask = Mode::from_bits(umask).with_context(|| "invalid bits")?;
                nix::sys::stat::umask(mask);
            }

            let uid = Uid::from_raw(proc.user().uid());
            let gid = Gid::from_raw(proc.user().gid());
            unistd::setresuid(uid, uid, uid).with_context(|| "failed to set uid")?;
            unistd::setresgid(gid, gid, gid).with_context(|| "failed to set gid")?;
            build_context(spec, config, &namespaces);

            listener.wait()?;

            config
                .run_create_container_hooks()
                .with_context(|| "StartContainer hooks")?;

            config.state.set_status(ContainerStatus::Running);
            config
                .run_start_container_hooks()
                .with_context(|| "StartContainer hooks")?;

            set_capabilities(caps::CapSet::Effective, &caps::all())
                .with_context(|| "failed to reset effective capabilities")?;
            if let Some(caps) = proc.capabilities() {
                drop_privileges(caps).with_context(|| "failed to drop capabilities")?;
            }

            run_sandbox()?;
            std::process::exit(0);
        }
        pid if pid > 0 => {
            pid_sender.send_pid(pid)?;
            config
                .run_create_runtime_hooks()
                .with_context(|| "StartContainer hooks")?;
            waitpid(Pid::from_raw(pid), None).with_context(|| "failed to wait")?;
            let cgroup_path = get_cgroup_path(linux.cgroups_path(), config.state.container_id());
            end_notifier
                .notify(b"sandbox execution ended!")
                .with_context(|| "failed to notify the end of execution")?;
            logger::debug!("cleaning up pid: {}", pid);
            cleanup_sandbox(config, &cgroup_path, dir).with_context(|| "cleanup failed")?;
            logger::debug!("clean up done for pid: {}", pid);
            Ok(())
        }
        _ => bail!("failed to fork"),
    }
}

fn run_sandbox() -> anyhow::Result<()> {
    let mut syscall_latencies: BTreeMap<Duration, (usize, usize)> = BTreeMap::new();
    let mut syscall_counter = 0usize;

    stub_init().with_context(|| "Failed to stub_init")?;

    let pid =
        unsafe { libc::syscall(libc::SYS_clone, libc::SIGCHLD | libc::CLONE_FILES, 0, 0) } as i32;
    match pid {
        0 => {
            let stub_addr = *STUB_START.lock().unwrap();
            unsafe {
                let stub: extern "C" fn() = std::mem::transmute(stub_addr as *mut u8);
                stub();
            }
            unreachable!("child stub should is an infinite loop");
        }
        pid if pid > 0 => {
            let pid = Pid::from_raw(pid);
            logger::info!("running child process: {:?}", pid);
            {
                let mut ctx = context::context_mut();
                ctx.set_tid(pid);
            }
            match waitpid(pid, Some(WaitPidFlag::__WALL | WaitPidFlag::WUNTRACED))
                .expect("waitpid failed")
            {
                WaitStatus::Stopped(_, sig) if sig == Signal::SIGSTOP => (),
                e => panic!("unexpected signal before attach: {:?}", e),
            }
            ptrace::attach(pid).expect("PTRACE_ATTACH failed");
            match waitpid(pid, Some(WaitPidFlag::__WALL)).expect("waitpid failed") {
                WaitStatus::Stopped(_, sig) if sig == Signal::SIGSTOP => (),
                e => panic!("unexpected signal after attach: {:?}", e),
            }

            {
                let ctx = context::context();
                let mut task = ctx.task_mut();
                task.grab_init_regs();
            }

            ptrace::setoptions(
                pid,
                ptrace::Options::PTRACE_O_EXITKILL
                    | ptrace::Options::PTRACE_O_TRACEEXEC
                    | ptrace::Options::PTRACE_O_TRACESYSGOOD,
            )
            .expect("PTRACE_SETOPTIONS failed");

            let arch_context = {
                let ctx = context::context();
                let mut task = ctx.task_mut();
                let extra_auxv = HashMap::new();
                task.load(
                    ctx.executable_path(),
                    ctx.argv().clone(),
                    ctx.envv(),
                    &extra_auxv,
                )
                .expect("Task::load() failed")
            };
            {
                let ctx = &*context::context();
                let address_space = ctx.platform().new_address_space(ctx);
                ctx.task().set_address_space(address_space);
            }
            {
                let ctx = context::context();
                let mut task = ctx.task_mut();
                task.set_arch_context(arch_context);
            }

            logger::info!("applying seccomp filters..");
            apply_filters().expect("Failed to install seccomp filter");
            logger::info!("applied seccomp filters");

            let mut last_segv_addr = None;
            let mut last_segv_ip = None;
            loop {
                {
                    let ctx = &*context::context();
                    let mut task = ctx.task_mut();
                    let mut regs = task.regs();
                    task.reset_sysemu_regs(&mut regs);
                    ctx.ptrace_set_regs(regs).expect("PTRACE_SETREGS failed");
                }
                ptrace::sysemu(pid, None).expect("PTRACE_SYSEMU failed");
                match waitpid(pid, Some(WaitPidFlag::__WALL | WaitPidFlag::WUNTRACED))
                    .expect("wait failed")
                {
                    WaitStatus::PtraceSyscall(_) => {
                        let mut regs = utils::init_libc_regs();
                        {
                            let ctx = context::context();
                            let task = ctx.task();
                            let pid = ctx.tid();
                            task.ptrace_get_regs(&mut regs, pid)
                                .expect("PTRACE_GETREGS failed");
                        }
                        let start = std::time::Instant::now();
                        let should_exit = syscalls::should_exit(regs.orig_rax as i64);
                        syscall_counter += 1;
                        regs.rax = match syscalls::perform(&mut regs, syscall_counter) {
                            Ok(n) => {
                                let elapsed = start.elapsed();
                                logger::info!(
                                    "success: {:#x} ({}) (Elapsed: {:?})\n",
                                    n,
                                    n,
                                    elapsed
                                );
                                syscall_latencies
                                    .insert(elapsed, (syscall_counter, regs.orig_rax as usize));
                                n as u64
                            }
                            Err(err) => {
                                logger::info!("failed: {} (Elapsed: {:?})\n", err, start.elapsed());
                                -err.code() as u64
                            }
                        };
                        if should_exit {
                            logger::info!("task exiting");
                            break;
                        }
                        {
                            let ctx = context::context();
                            let mut task = ctx.task_mut();
                            task.set_regs(regs);
                            ctx.ptrace_set_regs(regs).expect("PTRACE_SETREGS failed");
                        }
                    }
                    WaitStatus::Stopped(_, sig) => match sig {
                        Signal::SIGSEGV => {
                            let ctx = context::context();
                            {
                                let regs = ptrace::getregs(pid).expect("PTRACE_GETREGS failed");
                                let mut task = ctx.task_mut();
                                task.set_regs(regs);
                                if task.handle_possible_cpuid_instruction() {
                                    continue;
                                }
                            }
                            let sig_info = ptrace::getsiginfo(pid).expect("getsiginfo failed");
                            // just assume page fault for now.
                            let addr = Addr(unsafe { sig_info.si_addr() } as u64);

                            let rip = {
                                let task = ctx.task();
                                task.regs().rip
                            };

                            // FIXME: properly set read and write permissions
                            let at = AccessType {
                                read: true,
                                write: last_segv_addr == Some(addr) && last_segv_ip == Some(rip),
                                execute: addr.0 == rip,
                            };
                            last_segv_addr = Some(addr);
                            last_segv_ip = Some(rip);

                            let mm = ctx.memory_manager();
                            let mut mm = mm.borrow_mut();
                            match mm.handle_user_fault(addr, at) {
                                Ok(()) => {
                                    logger::info!("handled user fault: {:?} {}", at, addr);
                                    continue;
                                }
                                Err(e) => {
                                    logger::warn!(
                                        "handle_user_fault at {} failed (ignoring for now) {:?}: {:?}",
                                        addr,
                                        sig_info,
                                        e
                                    );
                                    let regs = ptrace::getregs(pid).expect("PTRACE_GETREGS failed");
                                    logger::warn!(
                                        "handle_user_fault failed and regs are: {:?}",
                                        regs
                                    );
                                    break;
                                }
                            }
                        }
                        Signal::SIGTRAP => continue,
                        e => bail!("unhandled signal at {}:{}: {:?}", file!(), line!(), e),
                    },
                    WaitStatus::Exited(_, _) => {
                        logger::debug!("exited");
                        break;
                    }
                    WaitStatus::PtraceEvent(_, sig, _) => {
                        bail!(
                            "traced process {:?} is stopped by a \"PTRACE_EVENT_*\" event {}",
                            pid,
                            sig
                        );
                    }
                    WaitStatus::Signaled(_, sig, _) => bail!("tracee killed by the signal {}", sig),
                    WaitStatus::StillAlive => bail!("still alive"),
                    WaitStatus::Continued(_) => bail!("continued"),
                }
            }
        }
        _ => bail!("failed to fork"),
    }

    logger::info!("Slowest syscalls");
    for (duration, (no, syscallno)) in syscall_latencies.iter().rev().take(10) {
        logger::info!("{} (#{}): {:?}", syscallno, no, duration);
    }

    // eprintln!("### STDOUT ###");
    if let Ok(stdout) = get_stdout() {
        print!("{}", stdout);
    }
    // eprintln!("### STDERR ###");
    if let Ok(stderr) = get_stderr() {
        eprint!("{}", stderr);
    }

    logger::info!("task successfully exited!");

    Ok(())
}

fn cleanup_sandbox<P: AsRef<Path>>(
    config: &mut SentinelConfig,
    cgroups_path: &PathBuf,
    container_root: P,
) -> anyhow::Result<()> {
    let mut errors = Vec::new();
    let cgroup_manager = libcgroups::common::create_cgroup_manager(
        cgroups_path,
        false,
        config.state.container_id(),
    )?;
    logger::debug!("removing cgroup");
    if let Err(e) = cgroup_manager.remove().context("failed to remove cgroup") {
        errors.push(e.to_string());
    }
    logger::debug!("removed cgroup");
    config.state.set_status(ContainerStatus::Stopped);
    config
        .run_poststop_hooks()
        .with_context(|| "failed to run poststop hooks")?;
    if container_root.as_ref().exists() {
        logger::debug!("deleting container root: {:?}", container_root.as_ref());
        if let Err(e) =
            std::fs::remove_dir_all(container_root).context("failed to remove container dir")
        {
            errors.push(e.to_string());
        }
    }
    if !errors.is_empty() {
        anyhow::bail!("failed to cleanup: {}", errors.join(";"));
    }
    Ok(())
}

fn build_context(spec: &Spec, config: &SentinelConfig, namespace: &SentinelNamespaces) {
    let uid = spec.process().as_ref().unwrap().user().uid();
    let gid = spec.process().as_ref().unwrap().user().gid();
    let creds = Credentials::new_user(
        Kuid(uid),
        Kgid(gid),
        None,
        Rc::new(UserNamespace::new_root()),
    );
    let kernel = Kernel::load();
    context::init_context(
        RwLock::new(LimitSet::default()),
        creds,
        kernel,
        spec,
        namespace,
        config,
        spec.process().as_ref().unwrap().args().as_ref().unwrap(),
    )
    .expect("failed to initialize the context");
}

fn apply_rest_namespaces(ns: &SentinelNamespaces, spec: &Spec) -> anyhow::Result<()> {
    ns.apply(|t| t != LinuxNamespaceType::User && t != LinuxNamespaceType::Pid)
        .with_context(|| "failed to apply namespaces")?;

    if let Some(uts) = ns.get(LinuxNamespaceType::Uts) {
        if uts.path().is_none() {
            if let Some(hostname) = spec.hostname() {
                unistd::sethostname(hostname)?;
            }
        }
    }
    Ok(())
}

/// Retrieve the default filters containing the syscall rules required by `Firecracker`
/// to function. The binary file is generated via the `build.rs` script of this crate.
fn apply_filters() -> anyhow::Result<()> {
    let bytes: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/basic_seccomp_filter.bpf"));
    let filters = deserialize_binary(bytes, DESERIALIZATION_BYTES_LIMIT)
        .map_err(|e| anyhow!("failed to get filter: {}", e))?;
    seccompiler::apply_filter(&filters.get("vmm").unwrap().clone())
        .map_err(|e| anyhow!("failed to apply filter: {}", e))
}

pub fn get_stdout() -> anyhow::Result<String> {
    get_output(1)
}

pub fn get_stderr() -> anyhow::Result<String> {
    get_output(2)
}

fn get_output(fd: i32) -> anyhow::Result<String> {
    use mem::IoSequence;

    let ctx = &*context::context();
    let mut task = ctx.task_mut();
    let file = task
        .get_file(fd)
        .ok_or_else(|| SysError::new(libc::EINVAL))?;

    let size = {
        let file = file.borrow();
        let uattr = file.unstable_attr()?;
        uattr.size
    };

    let mut rbuf = vec![0; size as usize];

    let n = file
        .borrow()
        .preadv(&mut IoSequence::bytes_sequence(&mut rbuf), 0, ctx)?;

    match std::str::from_utf8(&rbuf[..n]) {
        Ok(s) => Ok(s.to_string()),
        Err(err) => Err(anyhow::anyhow!("error occurred: {:?}", err)),
    }
}
