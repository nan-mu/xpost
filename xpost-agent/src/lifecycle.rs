use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::config::{Build, Config, Location, digest};

pub const INSTANCE_HEADER: &str = "x-xpost-instance";
pub const START_ID: &str = "XPOST_AGENT_INTERNAL_START_ID";
const START_TIMEOUT: Duration = Duration::from_secs(15);
const STOP_TIMEOUT: Duration = Duration::from_secs(15);
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(40);
const LOCAL: &str = "http://xpost-agent";
const USB_LOCK: &str = "/tmp/xpost-agent-usb.lock";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Instance {
    pub id: Uuid,
    pub pid: u32,
    pub endpoint: String,
    pub listen: SocketAddr,
    pub config: Config,
    pub build: Build,
    pub daemon_log: Option<PathBuf>,
    pub managed: bool,
}

impl Instance {
    pub fn new(id: Uuid, listen: SocketAddr, config: Config, build: Build, managed: bool) -> Self {
        let mut target = listen;
        if listen.ip().is_unspecified() {
            target.set_ip(match listen.ip() {
                IpAddr::V4(_) => Ipv4Addr::LOCALHOST.into(),
                IpAddr::V6(_) => Ipv6Addr::LOCALHOST.into(),
            });
        }
        Self {
            id,
            pid: std::process::id(),
            endpoint: format!("http://{target}"),
            listen,
            daemon_log: managed.then(|| log_path(&config, id)),
            config,
            build,
            managed,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Status {
    pub state: Phase,
    pub dut: String,
    pub instance: Instance,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Starting,
    Running,
    Stopping,
}

pub struct RuntimeDir {
    path: PathBuf,
    worktree: PathBuf,
}

impl RuntimeDir {
    pub fn open(worktree: &Path) -> Result<Self> {
        use std::os::unix::ffi::OsStrExt;

        // Keep the Unix socket path below sockaddr_un's limit.
        let user_dir = PathBuf::from(format!("/tmp/xpost-agent-{}", unsafe { libc::geteuid() }));
        private_dir(&user_dir)?;
        let hash = digest(worktree.as_os_str().as_bytes());
        let path = user_dir.join(&hash[..32]);
        private_dir(&path)?;
        Ok(Self {
            path,
            worktree: worktree.to_owned(),
        })
    }

    pub fn socket(&self) -> PathBuf {
        self.path.join("control.sock")
    }

    // Call with the Worktree instance lock held.
    pub fn clear_socket(&self) -> Result<()> {
        match fs::remove_file(self.socket()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("remove control socket"),
        }
    }

    pub fn remember(&self, instance: &Instance) -> Result<()> {
        let temp = self.path.join(format!("{}.json", instance.id));
        fs::write(&temp, serde_json::to_vec(instance)?)?;
        fs::rename(temp, self.path.join("last.json"))?;
        Ok(())
    }

    fn last(&self) -> Result<Instance> {
        let instance: Instance = serde_json::from_slice(&fs::read(self.path.join("last.json"))?)?;
        self.verify(&instance)?;
        Ok(instance)
    }

    fn verify(&self, instance: &Instance) -> Result<()> {
        ensure!(
            instance.config.worktree == self.worktree,
            "daemon Worktree identity mismatch"
        );
        Ok(())
    }

    pub fn claim_instance(&self) -> Result<Option<File>> {
        take_lock(&self.path.join("instance.lock"), 0o600)
    }

    pub fn claim_daemon(&self) -> Result<File> {
        // A concurrent status probe may briefly acquire a free instance lock.
        let deadline = Instant::now() + Duration::from_millis(200);
        loop {
            if let Some(lock) = self.claim_instance()? {
                return Ok(lock);
            }
            ensure!(Instant::now() < deadline, "Worktree already has a daemon");
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn claim_command(&self) -> Result<File> {
        let deadline = Instant::now() + START_TIMEOUT + STOP_TIMEOUT;
        loop {
            if let Some(lock) = take_lock(&self.path.join("command.lock"), 0o600)? {
                return Ok(lock);
            }
            ensure!(
                Instant::now() < deadline,
                "another lifecycle command is still running"
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    pub fn query(&self) -> Result<Option<Status>> {
        // The lock is authoritative for absence. A stale socket or PID cannot
        // make a stopped daemon appear live.
        if self.claim_instance()?.is_some() {
            return Ok(None);
        }
        let status: Status = self
            .client()?
            .get(format!("{LOCAL}/control/status"))
            .send()
            .context("daemon holds the Worktree lock but its control socket is unavailable")?
            .error_for_status()?
            .json()
            .context("decode daemon status")?;
        self.verify(&status.instance)?;
        Ok(Some(status))
    }

    fn client(&self) -> Result<Client> {
        Ok(crate::client::builder(QUERY_TIMEOUT)
            .unix_socket(self.socket())
            .build()?)
    }

    pub fn status(&self) -> Result<()> {
        match self.query() {
            Ok(Some(status)) => print_json(&status),
            Ok(None) => print_json(&json!({
                "state": "stopped", "dut": "unknown", "worktree": self.worktree
            })),
            Err(error) => {
                print_json(&json!({
                    "state": "abnormal", "dut": "unknown", "worktree": self.worktree,
                    "error": format!("{error:#}")
                }))?;
                Err(error)
            }
        }
    }

    pub fn logs(&self) -> Result<()> {
        let instance = match self.query() {
            Ok(Some(status)) => status.instance,
            _ => self
                .last()
                .context("no saved daemon log for this Worktree")?,
        };
        let path = instance
            .daemon_log
            .context("foreground daemon logs go to its terminal")?;
        std::io::copy(
            &mut File::open(&path).with_context(|| format!("read {}", path.display()))?,
            &mut io::stdout().lock(),
        )?;
        Ok(())
    }

    pub fn start(
        &self,
        location: &Location,
        config: &Config,
        build: &Build,
        restart: bool,
    ) -> Result<()> {
        let _command = self.claim_command()?;
        if restart {
            self.stop_managed()?;
        }
        if let Some(status) = self.query()? {
            let current = &status.instance;
            ensure!(
                current.managed && status.state == Phase::Running,
                "Worktree already has a {:?} or foreground daemon",
                status.state
            );
            ensure!(
                current.build.image_id == build.image_id && current.config == *config,
                "running daemon differs from this invocation. Run an explicit restart.\nRunning: {}\nRequested: {}",
                serde_json::to_string(current)?,
                serde_json::to_string(&json!({"config": config, "build": build}))?
            );
            return print_json(&status);
        }

        let id = Uuid::new_v4();
        let log_path = log_path(config, id);
        let log = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&log_path)?;
        let mut child = Command::new(&build.executable);
        child
            .arg("serve")
            .arg("--bind")
            .arg(&config.bind)
            .arg("--log-dir")
            .arg(&config.log_dir)
            .env("XPOST_ROOT", &config.root)
            .env("XPOST_PROFILES", &config.profiles)
            .env(START_ID, id.to_string())
            .current_dir(&location.cwd)
            .stdin(Stdio::piped())
            .stdout(log.try_clone()?)
            .stderr(log);
        // SAFETY: setsid is async-signal-safe and detaches the child from the
        // launching shell's process group and controlling terminal.
        unsafe {
            child.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = child.spawn().context("launch daemon")?;
        let deadline = Instant::now() + start_timeout();
        let prepared = (|| {
            let status = self.wait_for(&mut child, id, Phase::Starting, deadline)?;
            ensure!(
                status.instance.build.image_id == build.image_id
                    && status.instance.config == *config,
                "daemon inputs changed during startup"
            );
            let response = crate::client::builder(QUERY_TIMEOUT)
                .build()?
                .get(format!("{}/health", status.instance.endpoint))
                .send()?
                .error_for_status()?;
            ensure!(
                response
                    .headers()
                    .get(INSTANCE_HEADER)
                    .and_then(|v| v.to_str().ok())
                    == Some(&id.to_string()),
                "HTTP endpoint did not identify this daemon"
            );
            Ok::<_, anyhow::Error>(())
        })();

        if let Err(error) = prepared {
            // The child cannot enter Running before receiving the commit byte.
            self.abort_start(&mut child);
            let diagnostic = startup_diagnostic(&log_path);
            return Err(error.context(format!(
                "daemon startup failed{diagnostic}. Log: {}",
                log_path.display()
            )));
        }
        let mut stdin = child.stdin.take().context("daemon startup pipe missing")?;
        let activation = stdin.write_all(b"1");
        drop(stdin);
        if let Err(error) = activation {
            self.abort_start(&mut child);
            return Err(error)
                .with_context(|| format!("activate daemon. Log: {}", log_path.display()));
        }
        // The child may now accept device work. Use the normal stop protocol
        // for cleanup if the final acknowledgement is unavailable.
        match self.wait_for(
            &mut child,
            id,
            Phase::Running,
            Instant::now() + QUERY_TIMEOUT,
        ) {
            Ok(status) => print_json(&status),
            Err(error) => {
                let cleanup = self.stop_instance(id);
                Err(error.context(format!(
                    "confirm daemon activation. Cleanup: {cleanup:?}. Log: {}",
                    log_path.display()
                )))
            }
        }
    }

    fn abort_start(&self, child: &mut Child) {
        let _ = child.kill();
        let _ = child.wait();
        if let Ok(Some(_lock)) = self.claim_instance() {
            let _ = self.clear_socket();
        }
    }

    fn wait_for(
        &self,
        child: &mut Child,
        id: Uuid,
        phase: Phase,
        deadline: Instant,
    ) -> Result<Status> {
        loop {
            if let Some(exit) = child.try_wait()? {
                bail!("daemon exited with {exit}");
            }
            if let Ok(Some(status)) = self.query()
                && status.instance.id == id
                && status.state == phase
            {
                return Ok(status);
            }
            ensure!(Instant::now() < deadline, "daemon readiness timed out");
            thread::sleep(POLL_INTERVAL);
        }
    }

    pub fn stop(&self) -> Result<()> {
        let _command = self.claim_command()?;
        self.stop_managed()?;
        print_json(&json!({
            "state": "stopped", "dut": "unknown", "worktree": self.worktree
        }))
    }

    fn stop_managed(&self) -> Result<()> {
        if let Some(status) = self.query()? {
            ensure!(
                status.instance.managed,
                "stop the foreground daemon from its terminal"
            );
            self.stop_instance(status.instance.id)?;
        }
        Ok(())
    }

    fn stop_instance(&self, id: Uuid) -> Result<()> {
        let response = self
            .client()?
            .post(format!("{LOCAL}/control/stop"))
            .json(&json!({ "id": id }))
            .send()?;
        let code = response.status();
        ensure!(
            code.is_success(),
            "daemon refused stop ({code}): {}",
            response.text()?
        );
        let deadline = Instant::now() + STOP_TIMEOUT;
        while self.claim_instance()?.is_none() {
            ensure!(Instant::now() < deadline, "daemon is still stopping");
            thread::sleep(POLL_INTERVAL);
        }
        Ok(())
    }
}

pub fn print_json(value: &impl Serialize) -> Result<()> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    serde_json::to_writer(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

fn private_dir(path: &Path) -> Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e).with_context(|| format!("create {}", path.display())),
    }
    let info = fs::symlink_metadata(path)?;
    ensure!(
        info.is_dir() && info.uid() == unsafe { libc::geteuid() } && info.mode() & 0o077 == 0,
        "unsafe runtime directory {}",
        path.display()
    );
    Ok(())
}

fn take_lock(path: &Path, mode: u32) -> Result<Option<File>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("open lock {}", path.display()))?;
    ensure!(
        file.metadata()?.is_file(),
        "invalid lock file {}",
        path.display()
    );
    match unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } {
        0 => Ok(Some(file)),
        _ => {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                Ok(None)
            } else {
                Err(error.into())
            }
        }
    }
}

pub fn claim_device(path: Option<&Path>) -> Result<File> {
    // The production daemon auto-selects one USB serial device. Serialize that
    // selection across this host, including aliases and reconnects.
    let path = path.unwrap_or(Path::new(USB_LOCK));
    take_lock(path, 0o666)?
        .with_context(|| format!("DUT is already held by another daemon ({})", path.display()))
}

fn log_path(config: &Config, id: Uuid) -> PathBuf {
    config.log_dir.join(format!("daemon-{id}.log"))
}

fn start_timeout() -> Duration {
    #[cfg(feature = "test-device")]
    if let Ok(value) = std::env::var("XPOST_AGENT_TEST_START_TIMEOUT_MS")
        && let Ok(ms) = value.parse()
    {
        return Duration::from_millis(ms);
    }
    START_TIMEOUT
}

fn startup_diagnostic(log: &Path) -> String {
    fs::read_to_string(log)
        .ok()
        .and_then(|text| {
            text.lines()
                .last()
                .filter(|line| line.starts_with("xpost-agent: "))
                .map(str::to_owned)
        })
        .map_or_else(String::new, |line| format!(" ({line})"))
}
