//! Shared USB session + profiles for HTTP `/v1` and MCP `/mcp`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};
use serde::Deserialize;
use xpost_serial::{Chip, FileSink, Session};

use crate::util::{eprint_line, ts};

/// Don't reprint noisy COUNTERS / `map dump` lines more often than this.
pub const NOISY_LOG_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug, Deserialize)]
struct ProfilesFile {
    #[serde(default, rename = "profile")]
    profiles: Vec<ProfileEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProfileEntry {
    pub id: String,
    pub bin: String,
    pub partition_table: String,
    #[serde(default)]
    pub xdp_elf: Option<String>,
    #[serde(default)]
    pub xdp_name: Option<String>,
    /// Optional argv relative to workspace root, run before flash.
    #[serde(default)]
    pub build: Option<Vec<String>>,
}

pub struct AgentState {
    pub session: Session<FileSink>,
    pub profiles: Vec<ProfileEntry>,
    pub root: PathBuf,
    pub last_noisy_log: Option<Instant>,
    /// Latched after UART shows `xpost ready`; cleared on flash / reset_to_app.
    pub dut_ready: bool,
    pub dut_ready_line: Option<String>,
}

pub type SharedState = Arc<Mutex<AgentState>>;

pub fn open(port: Option<&Path>, log: &Path) -> Result<SharedState> {
    let sink = FileSink::create(log, true)
        .with_context(|| format!("create serial log {}", log.display()))?;
    eprint_line(&format!(
        "[{}] xpost-agent serial log {}",
        ts(),
        sink.path().display()
    ));
    let session = Session::open(port, Chip::Esp32c3, sink).context("open session")?;
    let root = workspace_root();
    let profiles = load_profiles(&root)?;
    eprint_line(&format!(
        "[{}] xpost-agent profiles={} from {}",
        ts(),
        profiles.len(),
        root.join("profiles.toml").display()
    ));
    for p in &profiles {
        let xdp = match (&p.xdp_name, &p.xdp_elf) {
            (Some(n), Some(e)) => format!(" xdp={n} ({e})"),
            (None, Some(e)) => format!(" xdp_elf={e}"),
            _ => String::new(),
        };
        let build = p
            .build
            .as_ref()
            .map(|a| format!(" build=[{}]", a.join(" ")))
            .unwrap_or_default();
        eprint_line(&format!(
            "[{}]   • {} → {}{xdp}{build}",
            ts(),
            p.id,
            p.bin
        ));
    }
    Ok(Arc::new(Mutex::new(AgentState {
        session,
        profiles,
        root,
        last_noisy_log: None,
        dut_ready: false,
        dut_ready_line: None,
    })))
}

fn workspace_root() -> PathBuf {
    if let Ok(p) = std::env::var("XPOST_ROOT") {
        return PathBuf::from(p);
    }
    let mut cur = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for _ in 0..6 {
        if cur.join("profiles.toml").is_file() {
            return cur;
        }
        if !cur.pop() {
            break;
        }
    }
    PathBuf::from(".")
}

fn load_profiles(root: &Path) -> Result<Vec<ProfileEntry>> {
    let path = std::env::var("XPOST_PROFILES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root.join("profiles.toml"));
    let text = fs::read_to_string(&path)
        .with_context(|| format!("read profiles {}", path.display()))?;
    let file: ProfilesFile = toml::from_str(&text)
        .with_context(|| format!("parse profiles {}", path.display()))?;
    Ok(file.profiles)
}
