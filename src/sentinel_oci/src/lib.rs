use std::{
    collections::HashMap,
    io::{ErrorKind, Write},
    os::unix::prelude::CommandExt,
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::Context;
use nix::{
    fcntl,
    sched::{setns, unshare, CloneFlags},
    sys::{signal, stat},
    unistd::{self, Pid},
};
use oci_spec::runtime::{Hook, Hooks, LinuxNamespace, LinuxNamespaceType, Spec};
use serde::{Deserialize, Serialize};

#[derive(Default)]
pub struct SentinelNamespaces {
    map: HashMap<LinuxNamespaceType, LinuxNamespace>,
}

impl From<Option<&Vec<LinuxNamespace>>> for SentinelNamespaces {
    fn from(ns: Option<&Vec<LinuxNamespace>>) -> Self {
        let map = ns
            .unwrap_or(&Vec::new())
            .iter()
            .map(|n| (n.typ(), n.clone()))
            .collect::<HashMap<_, _>>();
        Self { map }
    }
}

static ORDERED_NAMESPACES: &[LinuxNamespaceType] = &[
    LinuxNamespaceType::User,
    LinuxNamespaceType::Pid,
    LinuxNamespaceType::Uts,
    LinuxNamespaceType::Ipc,
    LinuxNamespaceType::Network,
    LinuxNamespaceType::Cgroup,
    LinuxNamespaceType::Mount,
];

impl SentinelNamespaces {
    pub fn get(&self, typ: LinuxNamespaceType) -> Option<&LinuxNamespace> {
        self.map.get(&typ)
    }

    pub fn apply<F: Fn(LinuxNamespaceType) -> bool>(&self, filter: F) -> anyhow::Result<()> {
        let to_enter = ORDERED_NAMESPACES
            .iter()
            .filter(|l| filter(**l))
            .filter_map(|l| self.map.get_key_value(l))
            .collect::<Vec<_>>();

        for (ns_type, ns) in to_enter {
            self.unshare_or_setns(ns)
                .with_context(|| format!("failed to enter {:?} namespace {:?}", ns_type, ns))?;
        }
        Ok(())
    }

    pub fn unshare_or_setns(&self, namespace: &LinuxNamespace) -> anyhow::Result<()> {
        match namespace.path() {
            Some(path) => {
                let fd = fcntl::open(path, fcntl::OFlag::empty(), stat::Mode::empty())
                    .with_context(|| "failed to open namespace")?;
                setns(fd, get_clone_flag(namespace.typ())).with_context(|| "failed to setns")?;
                unistd::close(fd).with_context(|| "failed to close")
            }
            None => {
                let flags = get_clone_flag(namespace.typ());
                unshare(flags).with_context(|| "failed to unshare")
            }
        }
    }
}

fn get_clone_flag(namespace_type: LinuxNamespaceType) -> CloneFlags {
    match namespace_type {
        LinuxNamespaceType::User => CloneFlags::CLONE_NEWUSER,
        LinuxNamespaceType::Pid => CloneFlags::CLONE_NEWPID,
        LinuxNamespaceType::Uts => CloneFlags::CLONE_NEWUTS,
        LinuxNamespaceType::Ipc => CloneFlags::CLONE_NEWIPC,
        LinuxNamespaceType::Network => CloneFlags::CLONE_NEWNET,
        LinuxNamespaceType::Cgroup => CloneFlags::CLONE_NEWCGROUP,
        LinuxNamespaceType::Mount => CloneFlags::CLONE_NEWNS,
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct SentinelConfig {
    hooks: Option<Hooks>,
    pub state: State,
}

static SENTINEL_CONFIG_NAME: &str = "sentinel_config.json";

macro_rules! define_run_hooks {
    ($fn_name:ident, $fn:ident) => {
        pub fn $fn_name(&self) -> anyhow::Result<()> {
            if let Some(hooks) = self.hooks.as_ref() {
                if let Some(hooks) = hooks.$fn().as_ref() {
                    for hook in hooks {
                        self.run_hook(&hook)
                            .with_context(|| "Failed to execute hook")?;
                    }
                }
            }
            Ok(())
        }
    };
}

impl SentinelConfig {
    pub fn from_spec(
        spec: &Spec,
        id: String,
        container_status: ContainerStatus,
        bundle: PathBuf,
    ) -> Self {
        let state = State {
            oci_version: spec.version().clone(),
            status: container_status,
            id,
            pid: None,
            bundle,
            annotations: spec.annotations().clone(),
        };
        Self {
            hooks: spec.hooks().clone(),
            state,
        }
    }

    pub fn save<P: AsRef<Path>>(&self, path: &P) -> anyhow::Result<()> {
        let file = std::fs::File::create(path.as_ref().join(SENTINEL_CONFIG_NAME))?;
        serde_json::to_writer(&file, self)?;
        Ok(())
    }

    pub fn load<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let file = std::fs::File::open(path.join(SENTINEL_CONFIG_NAME))?;
        let config = serde_json::from_reader(&file)
            .with_context(|| format!("Failed to load SentinelConfig from {:?}", path))?;
        Ok(config)
    }

    define_run_hooks!(run_prestart_hooks, prestart);
    define_run_hooks!(run_poststart_hooks, poststart);
    define_run_hooks!(run_create_runtime_hooks, create_runtime);
    define_run_hooks!(run_create_container_hooks, create_container);
    define_run_hooks!(run_start_container_hooks, start_container);
    define_run_hooks!(run_poststop_hooks, poststop);

    fn run_hook(&self, hook: &Hook) -> anyhow::Result<()> {
        let mut hook_command = std::process::Command::new(&hook.path());
        if let Some((arg0, args)) = hook.args().as_ref().and_then(|a| a.split_first()) {
            logger::debug!("run_hooks arg0: {:?}, args: {:?}", arg0, args);
            hook_command.arg0(arg0).args(args)
        } else {
            hook_command.arg0(&hook.path().display().to_string())
        };
        let envs = hook
            .env()
            .as_ref()
            .map(|e| utils::parse_env(e))
            .unwrap_or_default();
        let mut hook_proc = hook_command
            .env_clear()
            .envs(envs)
            .stdin(Stdio::piped())
            .spawn()
            .with_context(|| "Failed to execute hook")?;
        if let Some(stdin) = &mut hook_proc.stdin {
            let encoded_state = serde_json::to_string(&self.state)
                .with_context(|| "failed to encode container state")?;
            match stdin.write_all(encoded_state.as_bytes()) {
                Ok(()) => (),
                Err(e) if e.kind() == ErrorKind::BrokenPipe => (),
                Err(e) => {
                    let _ = signal::kill(
                        Pid::from_raw(hook_proc.id() as i32),
                        signal::Signal::SIGKILL,
                    );
                    anyhow::bail!("failed to write container state to stdin: {:?}", e);
                }
            }
        }
        let res = hook_proc
            .wait()
            .with_context(|| "Failed to wait hook execution")?;
        match res.code() {
            Some(0) => Ok(()),
            Some(exit_code) => {
                anyhow::bail!("failed to execute hook command. exit_code: {}", exit_code);
            }
            None => {
                anyhow::bail!("process is killed by signal");
            }
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    oci_version: String,
    id: String,
    status: ContainerStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<i32>,
    bundle: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    annotations: Option<HashMap<String, String>>,
}

impl State {
    pub fn set_status(&mut self, status: ContainerStatus) {
        self.status = status;
    }

    pub fn set_pid(&mut self, pid: Option<i32>) {
        self.pid = pid;
    }

    pub fn container_id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ContainerStatus {
    Creating,
    Created,
    Running,
    Stopped,
}

impl Default for ContainerStatus {
    fn default() -> Self {
        Self::Creating
    }
}
