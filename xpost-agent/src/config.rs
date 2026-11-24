use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::profiles::Profiles;

#[derive(Args, Debug)]
pub struct ServeArgs {
    /// HTTP address. Use 0.0.0.0:9240 only on a trusted network.
    #[arg(long, default_value = "127.0.0.1:9240")]
    pub bind: String,
    /// Serial transcript and daemon log directory, relative to the caller.
    #[arg(long, default_value = "target/xpost-agent")]
    pub log_dir: PathBuf,
}

pub struct Location {
    pub cwd: PathBuf,
    pub worktree: PathBuf,
    pub root: PathBuf,
}

impl Location {
    pub fn resolve() -> Result<Self> {
        let cwd = std::env::current_dir()?.canonicalize()?;
        let discovered = cwd
            .ancestors()
            .take(6)
            .find(|p| p.join("profiles.toml").is_file());
        let root = match std::env::var_os("XPOST_ROOT") {
            Some(root) => cwd.join(root),
            None => discovered.unwrap_or(&cwd).to_owned(),
        };
        let worktree = match discovered {
            Some(worktree) => worktree.to_owned(),
            None => root.canonicalize().context("resolve Worktree")?,
        };
        Ok(Self {
            cwd,
            worktree,
            root,
        })
    }

    pub fn config(&self, args: ServeArgs) -> Result<(Config, Profiles)> {
        let root = self.root.canonicalize().context("resolve XPOST_ROOT")?;
        let profiles = std::env::var_os("XPOST_PROFILES")
            .map(|path| self.cwd.join(path))
            .unwrap_or_else(|| root.join("profiles.toml"))
            .canonicalize()
            .context("resolve profiles.toml")?;
        let text = fs::read_to_string(&profiles)
            .with_context(|| format!("read {}", profiles.display()))?;
        let registry = Profiles::parse(&text)?;
        let log_dir = self.cwd.join(args.log_dir);
        fs::create_dir_all(&log_dir).with_context(|| format!("create {}", log_dir.display()))?;

        let config = Config {
            worktree: self.worktree.clone(),
            root,
            profiles,
            profiles_sha256: digest(text.as_bytes()),
            log_dir: log_dir.canonicalize()?,
            bind: args.bind,
        };
        Ok((config, registry))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Config {
    pub worktree: PathBuf,
    pub root: PathBuf,
    pub profiles: PathBuf,
    pub profiles_sha256: String,
    pub log_dir: PathBuf,
    pub bind: String,
}

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Build {
    pub executable: PathBuf,
    pub image_id: String,
    pub git_revision: Option<String>,
}

impl Build {
    pub fn current() -> Result<Self> {
        let executable = std::env::current_exe().context("resolve current executable")?;
        Ok(Self {
            executable,
            image_id: image_id()?,
            git_revision: option_env!("XPOST_BUILD_REVISION").map(str::to_owned),
        })
    }
}

#[cfg(target_os = "linux")]
fn image_id() -> Result<String> {
    // The kernel keeps this link attached to the loaded inode.
    let mut image = fs::File::open("/proc/self/exe").context("open current executable image")?;
    let mut hash = Sha256::new();
    std::io::copy(&mut image, &mut hash)?;
    Ok(format!("sha256:{:x}", hash.finalize()))
}

#[cfg(target_os = "macos")]
fn image_id() -> Result<String> {
    // XNU's CS_OPS_CDHASH returns the code-directory hash of the running
    // instance. arm64 macOS Cargo builds are ad-hoc signed by the linker.
    unsafe extern "C" {
        fn csops(
            pid: libc::pid_t,
            ops: libc::c_uint,
            addr: *mut libc::c_void,
            size: usize,
        ) -> libc::c_int;
    }
    const CS_OPS_CDHASH: libc::c_uint = 5;
    let mut hash = [0u8; 20];
    // SAFETY: PID 0 means self. The output buffer is writable for its length.
    let result = unsafe { csops(0, CS_OPS_CDHASH, hash.as_mut_ptr().cast(), hash.len()) };
    if result != 0 {
        return Err(std::io::Error::last_os_error())
            .context("read running Mach-O code-directory hash");
    }
    let hash = hash.iter().map(|b| format!("{b:02x}")).collect::<String>();
    Ok(format!("cdhash:{hash}"))
}
