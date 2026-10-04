//! Synchronous USB operations shared by HTTP and MCP.

use std::path::{Path, PathBuf};
use std::process::Command as ProcCommand;
use std::sync::Mutex;
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use xpost_serial::client::flash::xdp_name_from_file;
use xpost_serial::client::{Command, FlashOp, FlashResult, OkReply};

use crate::state::{AgentError, AgentState, NOISY_LOG_EVERY, ProfileEntry};
use crate::util::{file_size, fmt_bytes, fmt_dur, log_line, truncate};

fn maybe_log_noisy(state: &Mutex<AgentState>, noisy: bool) -> bool {
    if !noisy {
        return true;
    }
    let Ok(mut g) = state.lock() else {
        return true;
    };
    let due = g
        .last_noisy_log
        .map(|t| t.elapsed() >= NOISY_LOG_EVERY)
        .unwrap_or(true);
    if due {
        g.last_noisy_log = Some(Instant::now());
    }
    due
}

fn with_log(
    peer: &str,
    name: &str,
    summary: &str,
    noisy: bool,
    state: &Mutex<AgentState>,
    f: impl FnOnce() -> Result<String>,
) -> Result<String> {
    let log_this = maybe_log_noisy(state, noisy);
    let t0 = Instant::now();
    if log_this {
        log_line(peer, &format!("→ {name} {summary}"));
    }
    match f() {
        Ok(msg) => {
            if log_this {
                log_line(
                    peer,
                    &format!(
                        "← {name} ok in {} — {}",
                        fmt_dur(t0.elapsed()),
                        truncate(&msg, 120)
                    ),
                );
            }
            Ok(msg)
        }
        Err(e) => {
            log_line(
                peer,
                &format!("✗ {name} failed in {}: {e:#}", fmt_dur(t0.elapsed())),
            );
            Err(e)
        }
    }
}

fn lock(state: &Mutex<AgentState>) -> Result<std::sync::MutexGuard<'_, AgentState>> {
    state.lock().map_err(|_| anyhow::anyhow!("handler lock"))
}

fn req<'a>(g: &'a AgentState, cmd: &Command<'_>) -> Result<OkReply<'a>> {
    g.handler
        .request::<AgentError>(cmd)
        .map_err(|e| anyhow::anyhow!("{e}"))
}

pub fn get_status() -> String {
    "idle".into()
}

/// Ping settle: `ready` on OK ping, else `booting`.
pub fn dut_status(state: &Mutex<AgentState>) -> Result<String> {
    let g = lock(state)?;
    match req(&g, &Command::Ping) {
        Ok(OkReply::Ping) => Ok("ready".into()),
        Ok(_) => Ok("ready".into()),
        Err(e) => {
            let msg = format!("{e:#}");
            if msg.contains("timeout") || msg.contains("busy") || msg.contains("interrupted") {
                Ok("booting".into())
            } else {
                Ok(format!("booting ({msg})"))
            }
        }
    }
}

pub fn flash_fw(state: &Mutex<AgentState>, profile_id: &str, peer: &str) -> Result<String> {
    with_log(
        peer,
        "FlashFw",
        &format!("profile=`{profile_id}`"),
        false,
        state,
        || flash_fw_inner(state, profile_id, peer),
    )
}

fn flash_fw_inner(state: &Mutex<AgentState>, id: &str, peer: &str) -> Result<String> {
    let (root, entry) = {
        let g = lock(state)?;
        let entry = g
            .profiles
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .with_context(|| format!("unknown profile `{id}`"))?;
        (g.root.clone(), entry)
    };

    let build_dur = run_profile_build(&root, &entry, peer)?;

    let firmware = root.join(&entry.bin);
    let partitions = root.join(&entry.partition_table);
    if !firmware.is_file() {
        bail!(
            "firmware not found after build: {} (profile `{id}`)",
            firmware.display()
        );
    }
    if !partitions.is_file() {
        bail!("partitions not found: {}", partitions.display());
    }
    let fw_sz = file_size(&firmware).unwrap_or(0);
    log_line(
        peer,
        &format!(
            "[fw] profile `{id}`: {} ({})  [part] {}  (no xdp install)",
            firmware.display(),
            fmt_bytes(fw_sz),
            partitions.display(),
        ),
    );

    let g = lock(state)?;
    let t_flash = Instant::now();
    g.handler
        .flash(FlashOp::Fw {
            firmware: firmware.display().to_string(),
            partitions: partitions.display().to_string(),
        })
        .with_context(|| format!("flash fw profile `{id}`"))?;
    let flash_dur = t_flash.elapsed();
    Ok(format!(
        "flashed profile={id} fw={} build={} flash={}",
        fmt_bytes(fw_sz),
        fmt_dur(build_dur),
        fmt_dur(flash_dur)
    ))
}

fn run_profile_build(root: &Path, entry: &ProfileEntry, peer: &str) -> Result<Duration> {
    let Some(argv) = entry.build.as_ref() else {
        log_line(peer, &format!("build profile `{}`: (none)", entry.id));
        return Ok(Duration::ZERO);
    };
    if argv.is_empty() {
        return Ok(Duration::ZERO);
    }
    let (prog, args) = argv.split_first().unwrap();
    log_line(
        peer,
        &format!(
            "build profile `{}`: {} {}  (cwd {})",
            entry.id,
            prog,
            args.join(" "),
            root.display()
        ),
    );
    let t0 = Instant::now();
    let status = ProcCommand::new(prog)
        .args(args)
        .current_dir(root)
        .status()
        .with_context(|| format!("spawn build for profile `{}`", entry.id))?;
    let elapsed = t0.elapsed();
    if !status.success() {
        bail!(
            "build failed for profile `{}` in {} (status={status}): {} {}",
            entry.id,
            fmt_dur(elapsed),
            prog,
            args.join(" ")
        );
    }
    log_line(
        peer,
        &format!("build profile `{}` ok in {}", entry.id, fmt_dur(elapsed)),
    );
    Ok(elapsed)
}

pub fn flash_xdp(state: &Mutex<AgentState>, elf: &str, peer: &str) -> Result<String> {
    with_log(peer, "FlashXdp", &format!("elf={elf}"), false, state, || {
        let path = PathBuf::from(elf.trim());
        if !path.is_file() {
            bail!("elf not found: {}", path.display());
        }
        let name = xdp_name_from_file(&path)
            .with_context(|| format!("parse name from {}", path.display()))?;
        let sz = file_size(&path).unwrap_or(0);
        let g = lock(state)?;
        let outcome = g
            .handler
            .flash(FlashOp::Xdp {
                name: name.clone(),
                path: path.display().to_string(),
            })
            .with_context(|| format!("flash xdp `{name}`"))?;
        match outcome {
            FlashResult::Install(xpost_serial::InstallOutcome::Overwritten { offset }) => {
                Ok(format!(
                    "overwritten name={name} offset={offset:#x} size={}",
                    fmt_bytes(sz)
                ))
            }
            FlashResult::Install(xpost_serial::InstallOutcome::Created { offset }) => Ok(format!(
                "created name={name} offset={offset:#x} size={}",
                fmt_bytes(sz)
            )),
            FlashResult::Done => Ok(format!("done name={name} size={}", fmt_bytes(sz))),
            FlashResult::List(_) => bail!("unexpected list result from flash xdp"),
        }
    })
}

/// `names`: after normalizing body — `"*"` → clear all; otherwise named list.
pub fn clear_xdp(state: &Mutex<AgentState>, names: Vec<String>, peer: &str) -> Result<String> {
    let summary = if names.is_empty() {
        "*".into()
    } else {
        names.join(",")
    };
    with_log(peer, "ClearXdp", &summary, false, state, || {
        let g = lock(state)?;
        g.handler
            .flash(FlashOp::Clear { names })
            .context("clear xdp")?;
        Ok(format!("cleared {summary}"))
    })
}

pub fn get_xdp(state: &Mutex<AgentState>, peer: &str) -> Result<String> {
    with_log(peer, "GetXdp", "", false, state, || {
        let g = lock(state)?;
        match g.handler.flash(FlashOp::List).context("get xdp")? {
            FlashResult::List(entries) if entries.is_empty() => Ok("(empty cold table)".into()),
            FlashResult::List(entries) => {
                let mut lines = Vec::with_capacity(entries.len());
                for e in &entries {
                    lines.push(format!(
                        "name={} offset={:#x} len={:#x}",
                        e.name, e.offset, e.len
                    ));
                }
                Ok(lines.join("\n"))
            }
            FlashResult::Done | FlashResult::Install(_) => {
                bail!("unexpected flash result from get xdp")
            }
        }
    })
}

pub fn enable_xdp(state: &Mutex<AgentState>, peer: &str) -> Result<String> {
    with_log(peer, "EnableXdp", "", false, state, || {
        let g = lock(state)?;
        req(&g, &Command::EnableXdp)?;
        Ok("OK enable xdp".into())
    })
}

pub fn disable_xdp(state: &Mutex<AgentState>, peer: &str) -> Result<String> {
    with_log(peer, "DisableXdp", "", false, state, || {
        let g = lock(state)?;
        req(&g, &Command::DisableXdp)?;
        Ok("OK disable xdp".into())
    })
}

pub fn clear_map(state: &Mutex<AgentState>, name: &str, peer: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("empty map name");
    }
    with_log(peer, "ClearMap", name, false, state, || {
        let g = lock(state)?;
        req(&g, &Command::ClearMap { name })?;
        Ok("OK clear map".into())
    })
}

pub fn get_map(state: &Mutex<AgentState>, name: &str, peer: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("empty map name");
    }
    with_log(peer, "GetMap", name, true, state, || {
        let g = lock(state)?;
        match req(&g, &Command::GetMap { name })? {
            OkReply::GetMap { blob } => {
                let b64 = base64_encode(blob);
                Ok(format!("OK get map {} b64={b64}", blob.len()))
            }
            _ => bail!("unexpected reply for get map"),
        }
    })
}

pub struct MapKv {
    pub key: String,
    pub value: String,
}

pub fn update_map(
    state: &Mutex<AgentState>,
    name: &str,
    entries: &[MapKv],
    peer: &str,
) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("empty map name");
    }
    if entries.is_empty() {
        bail!("no map entries to update");
    }
    with_log(
        peer,
        "UpdateMap",
        &format!("{name} n={}", entries.len()),
        false,
        state,
        || {
            let g = lock(state)?;
            let mut n = 0usize;
            for e in entries {
                let key = e.key.as_bytes();
                let value = e.value.as_bytes();
                req(
                    &g,
                    &Command::UpdateMap {
                        name,
                        key,
                        value,
                    },
                )?;
                n += 1;
            }
            Ok(format!("OK update map n={n}"))
        },
    )
}

pub fn clear_perf(state: &Mutex<AgentState>, peer: &str) -> Result<String> {
    with_log(peer, "ClearPerf", "", false, state, || {
        let g = lock(state)?;
        req(&g, &Command::ClearPerf)?;
        Ok("OK clear perf".into())
    })
}

pub fn get_perf(state: &Mutex<AgentState>, peer: &str) -> Result<String> {
    with_log(peer, "GetPerf", "", true, state, || {
        let g = lock(state)?;
        match req(&g, &Command::GetPerf)? {
            OkReply::GetPerf { blob } => {
                let b64 = base64_encode(blob);
                Ok(format!("OK get perf {} b64={b64}", blob.len()))
            }
            _ => bail!("unexpected reply for get perf"),
        }
    })
}

pub fn get_serial(
    state: &Mutex<AgentState>,
    since_unix_ms: u64,
    until_unix_ms: u64,
    peer: &str,
) -> Result<String> {
    with_log(
        peer,
        "GetSerial",
        &format!("ms={since_unix_ms}..{until_unix_ms}"),
        false,
        state,
        || {
            let g = lock(state)?;
            // Touch UART briefly so late lines land in the transcript.
            let _ = g.handler.request::<AgentError>(&Command::Ping);
            let since = UNIX_EPOCH + Duration::from_millis(since_unix_ms);
            let until = UNIX_EPOCH + Duration::from_millis(until_unix_ms);
            let lines = g
                .handler
                .lines_in_range(since, until)
                .context("lines_in_range")?;
            Ok(format!(
                "OK get serial n={} since_ms={since_unix_ms} until_ms={until_unix_ms}\n{}",
                lines.len(),
                lines.join("\n")
            ))
        },
    )
}

/// Debug escape hatch: parse one App line → Command; first OK/ERR only.
/// Not the supported path for get map/perf framing.
pub fn run_cmd(state: &Mutex<AgentState>, cmd: &str, peer: &str) -> Result<String> {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        bail!("empty cmd");
    }
    let owned = cmd.to_string();
    with_log(peer, "Run", &format!("`{owned}`"), false, state, move || {
        let parsed = parse_app_command(&owned)?;
        let g = lock(state)?;
        match &parsed {
            ParsedCmd::Ping => {
                req(&g, &Command::Ping)?;
                Ok("OK ping".into())
            }
            ParsedCmd::EnableXdp => {
                req(&g, &Command::EnableXdp)?;
                Ok("OK enable xdp".into())
            }
            ParsedCmd::DisableXdp => {
                req(&g, &Command::DisableXdp)?;
                Ok("OK disable xdp".into())
            }
            ParsedCmd::ClearMap { name } => {
                req(&g, &Command::ClearMap { name })?;
                Ok("OK clear map".into())
            }
            ParsedCmd::GetMap { name } => match req(&g, &Command::GetMap { name })? {
                OkReply::GetMap { blob } => {
                    Ok(format!("OK get map {} b64={}", blob.len(), base64_encode(blob)))
                }
                _ => bail!("unexpected get map reply"),
            },
            ParsedCmd::UpdateMap { name, key, value } => {
                req(
                    &g,
                    &Command::UpdateMap {
                        name,
                        key: key.as_bytes(),
                        value: value.as_bytes(),
                    },
                )?;
                Ok("OK update map".into())
            }
            ParsedCmd::ClearPerf => {
                req(&g, &Command::ClearPerf)?;
                Ok("OK clear perf".into())
            }
            ParsedCmd::GetPerf => match req(&g, &Command::GetPerf)? {
                OkReply::GetPerf { blob } => {
                    Ok(format!("OK get perf {} b64={}", blob.len(), base64_encode(blob)))
                }
                _ => bail!("unexpected get perf reply"),
            },
        }
    })
}

enum ParsedCmd<'a> {
    Ping,
    EnableXdp,
    DisableXdp,
    ClearMap { name: &'a str },
    GetMap { name: &'a str },
    UpdateMap {
        name: &'a str,
        key: &'a str,
        value: &'a str,
    },
    ClearPerf,
    GetPerf,
}

fn parse_app_command(line: &str) -> Result<ParsedCmd<'_>> {
    let mut parts = line.split_whitespace();
    let verb = parts.next().context("empty command")?;
    let eq = |a: &str, b: &str| a.eq_ignore_ascii_case(b);

    if eq(verb, "ping") {
        if parts.next().is_some() {
            bail!("ERR bad-arity");
        }
        return Ok(ParsedCmd::Ping);
    }

    let target = parts.next().context("ERR unknown")?;
    if eq(verb, "enable") && eq(target, "xdp") {
        if parts.next().is_some() {
            bail!("ERR bad-arity");
        }
        return Ok(ParsedCmd::EnableXdp);
    }
    if eq(verb, "disable") && eq(target, "xdp") {
        if parts.next().is_some() {
            bail!("ERR bad-arity");
        }
        return Ok(ParsedCmd::DisableXdp);
    }
    if eq(verb, "clear") && eq(target, "perf") {
        if parts.next().is_some() {
            bail!("ERR bad-arity");
        }
        return Ok(ParsedCmd::ClearPerf);
    }
    if eq(verb, "get") && eq(target, "perf") {
        if parts.next().is_some() {
            bail!("ERR bad-arity");
        }
        return Ok(ParsedCmd::GetPerf);
    }
    if eq(verb, "clear") && eq(target, "map") {
        let name = parts.next().context("ERR bad-arity")?;
        if parts.next().is_some() {
            bail!("ERR bad-arity");
        }
        return Ok(ParsedCmd::ClearMap { name });
    }
    if eq(verb, "get") && eq(target, "map") {
        let name = parts.next().context("ERR bad-arity")?;
        if parts.next().is_some() {
            bail!("ERR bad-arity");
        }
        return Ok(ParsedCmd::GetMap { name });
    }
    if eq(verb, "update") && eq(target, "map") {
        let name = parts.next().context("ERR bad-arity")?;
        let key = parts.next().context("ERR bad-arity")?;
        let value = parts.next().context("ERR bad-arity")?;
        if parts.next().is_some() {
            bail!("ERR bad-arity");
        }
        return Ok(ParsedCmd::UpdateMap { name, key, value });
    }
    bail!("ERR unknown")
}

fn base64_encode(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0] as u32;
        let b = chunk.get(1).copied().unwrap_or(0) as u32;
        let c = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (a << 16) | (b << 8) | c;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(T[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(T[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}
