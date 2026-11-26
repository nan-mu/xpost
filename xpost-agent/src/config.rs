use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use sha2::{Digest, Sha256};

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
    pub worktree: PathBuf,
    root: PathBuf,
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
        Ok(Self { worktree, root })
    }

    pub fn root(&self) -> Result<PathBuf> {
        self.root.canonicalize().context("resolve XPOST_ROOT")
    }

    pub fn worktree_id(&self) -> String {
        use std::os::unix::ffi::OsStrExt;
        format!("{:x}", Sha256::digest(self.worktree.as_os_str().as_bytes()))
    }
}
