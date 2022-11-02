use std::{
    io::{Read, Write},
    str::FromStr,
};

use anyhow::Context;
use clap::{Arg, ArgMatches, Command};
use logger::LevelFilter;
use nix::sys::signal;
use oci_spec::runtime::Spec;
use sentinel_oci::{ContainerStatus, SentinelConfig};

use crate::{spawn_sandbox, NotifyListener, NotifySender};

pub fn oci_main() -> anyhow::Result<()> {
    let id_arg = Arg::new("id")
        .required(true)
        .takes_value(true)
        .help("Unique identifier");

    let bundle_arg = Arg::new("bundle")
        .default_value(".")
        .long("bundle")
        .short('b')
        .help("Directory containing config.json");

    let pid_arg = Arg::new("p")
        .takes_value(true)
        .long("pid-file")
        .short('p')
        .help("Additional location to write pid");

    let init_arg = Arg::new("n").long("no-init").short('n');

    let matches = Command::new("sentinel")
        .arg(Arg::new("v").multiple_occurrences(true).short('v'))
        .arg(Arg::new("log").long("log").takes_value(true))
        .arg(Arg::new("log-format").long("log-format").takes_value(true))
        .arg(
            Arg::new("r")
                .default_value("/run/sentinel")
                .help("Dir for state")
                .long("root")
                .short('r')
                .takes_value(true),
        )
        .subcommand(
            Command::new("create")
                .arg(&id_arg)
                .arg(&bundle_arg)
                .arg(&pid_arg)
                .arg(&init_arg),
        )
        .subcommand(Command::new("start").arg(&id_arg))
        .subcommand(
            Command::new("run")
                .arg(&id_arg)
                .arg(&bundle_arg)
                .arg(&pid_arg)
                .arg(&init_arg),
        )
        .subcommand(Command::new("delete").arg(&id_arg))
        .get_matches();

    let state_dir = matches.value_of("r").unwrap();
    match matches.subcommand() {
        Some(("create", create)) => oci_create(create, state_dir),
        Some(("start", start)) => oci_start(start, state_dir),
        Some(("run", run)) => oci_run(run, state_dir),
        Some(("delete", delete)) => oci_delete(delete, state_dir),
        e => anyhow::bail!("unknown subcommand for Sentinel {:?}", e),
    }
}

fn oci_create(matches: &ArgMatches, state_dir: &str) -> anyhow::Result<()> {
    let id = matches.value_of("id").unwrap();
    let bundle = matches.value_of("bundle").unwrap();
    std::env::set_current_dir(bundle)?;
    let spec = Spec::load("config.json")?;
    let log_level = match spec.process().as_ref().unwrap().env().as_ref() {
        Some(env) => {
            let level = env
                .iter()
                .find_map(|s| s.strip_prefix("SENTINEL_LOG_LEVEL="));
            match level {
                Some(level) => LevelFilter::from_str(level).unwrap_or(LevelFilter::Off),
                None => LevelFilter::Off,
            }
        }
        None => LevelFilter::Off,
    };
    logger::init(log_level).map_err(|_| anyhow::anyhow!("failed to set log level"))?;

    let dir = format!("{}/{}", state_dir, id);
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create directory {}", dir))?;

    let namespace_notifier_path = format!("{}/ns_notify.sock", dir);
    let notify = NotifyListener::new(format!("{}/notify.sock", dir))
        .context("failed to create NotifyListener")?;
    let namespace_sender = NotifySender::new(&namespace_notifier_path);
    let end_notifier = NotifySender::new(format!("{}/end.sock", dir));
    let mut config = SentinelConfig::from_spec(
        &spec,
        id.to_string(),
        ContainerStatus::Creating,
        bundle.to_string().into(),
    );
    let container_pid = spawn_sandbox(
        &notify,
        &namespace_sender,
        &end_notifier,
        &dir,
        &spec,
        &mut config,
    )
    .context("failed to spawn container process")?;
    config.state.set_pid(Some(container_pid));

    if let Some(pid_file) = matches.value_of("p") {
        let mut f = std::fs::File::create(pid_file)?;
        f.write_all(format!("{}", container_pid).as_bytes())?;
    }

    let listener = NotifyListener::new(&namespace_notifier_path).with_context(|| {
        format!(
            "failed to initialize listener from path {:?}",
            namespace_notifier_path
        )
    })?;
    listener
        .wait()
        .with_context(|| "failed to wait for namespace initialization")?;
    config.state.set_status(ContainerStatus::Created);
    config
        .run_create_runtime_hooks()
        .with_context(|| "CreateRuntime hooks")?;

    config.save(&dir)?;

    Ok(())
}

fn oci_start(matches: &ArgMatches, state_dir: &str) -> anyhow::Result<()> {
    let id = matches.value_of("id").unwrap();
    let dir = format!("{}/{}", state_dir, id);
    let config = SentinelConfig::load(&dir)?;

    config
        .run_prestart_hooks()
        .with_context(|| "failed to execute prestart hooks")?;

    std::env::set_current_dir(&dir)
        .with_context(|| format!("failed to set current directory to {}", dir))?;
    let sock_path = format!("{}/notify.sock", dir);
    let notify = NotifySender::new(&sock_path);
    notify
        .notify(b"start container!")
        .context("failed to notify start")?;
    if let Err(e) = config.run_poststart_hooks() {
        logger::warn!("failed to execute post start hooks: {:?}", e);
    }

    let end_listener =
        NotifyListener::new(format!("{}/end.sock", dir)).with_context(|| "end with notifier")?;
    end_listener.wait().with_context(|| "failed to wait")?;

    Ok(())
}

fn oci_run(matches: &ArgMatches, state_dir: &str) -> anyhow::Result<()> {
    oci_create(matches, state_dir).with_context(|| "run failed to create")?;
    oci_start(matches, state_dir).with_context(|| "run failed to start")
}

fn oci_delete(matches: &ArgMatches, state_dir: &str) -> anyhow::Result<()> {
    let id = matches.value_of("id").unwrap();
    let dir = format!("{}/{}", state_dir, id);
    nix::unistd::chdir(&*dir).with_context(|| "failed to chdir")?;

    let mut f = std::fs::File::open("process.pid").with_context(|| "process doesn't exist")?;
    let mut result = String::new();
    f.read_to_string(&mut result)
        .with_context(|| "failed to read file content of process.pid")?;
    let pid = result
        .parse::<i32>()
        .with_context(|| "failed to parse pid")?;
    let pid = nix::unistd::Pid::from_raw(pid);
    signal::kill(pid, None).with_context(|| "container process is still running")?;

    let mut f = std::fs::File::open("init.pid").with_context(|| "process doesn't exist")?;
    let mut result = String::new();
    f.read_to_string(&mut result)
        .with_context(|| "failed to read file content of process.pid")?;
    let pid = result
        .parse::<i32>()
        .with_context(|| "failed to parse pid")?;
    let pid = nix::unistd::Pid::from_raw(pid);
    signal::kill(pid, signal::Signal::SIGKILL).with_context(|| "failed to kill process")?;

    std::fs::remove_dir_all(&dir).with_context(|| "failed to remove all directories")?;
    Ok(())
}
