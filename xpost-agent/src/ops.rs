//! Synchronous USB operations shared by HTTP and MCP.

use std::path::Path;
use std::process::Command as ProcCommand;
use std::sync::Mutex;
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use xpost_serial::client::flash::xdp_name_from_file;
use xpost_serial::client::{Command, FlashOp, FlashResult, OkReply};

use crate::profiles::{BuildSpec, XdpProfile, abs_under_root};
use crate::state::{AgentError, AgentState, NOISY_LOG_EVERY};
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

/// Host wait for unsolicited `OK init` (firmware finished App init).
pub fn get_init(state: &Mutex<AgentState>, peer: &str) -> Result<String> {
    with_log(peer, "GetInit", "", false, state, || {
        let g = lock(state)?;
        req(&g, &Command::GetInit)?;
        Ok("OK init".into())
    })
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

pub fn flash_fw(state: &Mutex<AgentState>, name: &str, peer: &str) -> Result<String> {
    with_log(
        peer,
        "FlashFw",
        &format!("name=`{name}`"),
        false,
        state,
        || flash_fw_inner(state, name, peer),
    )
}

fn flash_fw_inner(state: &Mutex<AgentState>, name: &str, peer: &str) -> Result<String> {
    let (root, fw, linked_xdps) = {
        let g = lock(state)?;
        let fw = g
            .profiles
            .fw(name)
            .cloned()
            .with_context(|| format!("unknown fw `{name}`"))?;
        let mut linked = Vec::with_capacity(fw.xdp.len());
        for xn in &fw.xdp {
            let x = g
                .profiles
                .xdp(xn)
                .cloned()
                .with_context(|| format!("fw `{name}` lists missing xdp `{xn}`"))?;
            linked.push(x);
        }
        (g.root.clone(), fw, linked)
    };

    let build_dur = run_build(&root, &fw.build, &format!("fw `{name}`"), peer)?;

    let firmware = abs_under_root(&root, &fw.artifact);
    let partitions = abs_under_root(&root, &fw.partition_table);
    if !firmware.is_file() {
        bail!(
            "firmware not found after build: {} (fw `{name}`)",
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
            "[fw] `{name}`: {} ({})  [part] {}  co-flash xdp={}",
            firmware.display(),
            fmt_bytes(fw_sz),
            partitions.display(),
            linked_xdps.len(),
        ),
    );

    let t_flash = Instant::now();
    {
        let g = lock(state)?;
        g.handler
            .flash(FlashOp::Fw {
                firmware: firmware.display().to_string(),
                partitions: partitions.display().to_string(),
            })
            .with_context(|| format!("flash fw `{name}`"))?;
    }
    let mut flash_dur = t_flash.elapsed();

    // Profiles carry one cold image per fw. Stale names (e.g. just_tx → just_drop)
    // would append mid-sector and espflash erase_region rejects unaligned addresses.
    if !linked_xdps.is_empty() {
        log_line(peer, "clear xdp table before co-flash");
        let t_clear = Instant::now();
        {
            let g = lock(state)?;
            g.handler
                .flash(FlashOp::Clear {
                    names: Vec::new(),
                })
                .context("clear xdp before co-flash")?;
        }
        flash_dur += t_clear.elapsed();
    }

    let mut xdp_parts = Vec::new();
    for x in &linked_xdps {
        let part = flash_one_xdp(state, &root, x, peer)?;
        flash_dur += part.1;
        xdp_parts.push(part.0);
    }

    let mut msg = format!(
        "flashed fw={name} size={} build={} flash={}",
        fmt_bytes(fw_sz),
        fmt_dur(build_dur),
        fmt_dur(flash_dur)
    );
    if !xdp_parts.is_empty() {
        msg.push_str("; xdp=[");
        msg.push_str(&xdp_parts.join(", "));
        msg.push(']');
    }
    Ok(msg)
}

pub fn flash_xdp(state: &Mutex<AgentState>, name: &str, peer: &str) -> Result<String> {
    with_log(
        peer,
        "FlashXdp",
        &format!("name=`{name}`"),
        false,
        state,
        || {
            let (root, xdp) = {
                let g = lock(state)?;
                let xdp = g
                    .profiles
                    .xdp(name)
                    .cloned()
                    .with_context(|| format!("unknown xdp `{name}`"))?;
                // Ensure linked fw / partition table resolve (validated on load).
                let _ = g.profiles.partition_table_for_xdp(name)?;
                (g.root.clone(), xdp)
            };
            let (summary, _) = flash_one_xdp(state, &root, &xdp, peer)?;
            Ok(summary)
        },
    )
}

/// Build + flash one XDP profile. Returns (summary, flash duration).
fn flash_one_xdp(
    state: &Mutex<AgentState>,
    root: &Path,
    xdp: &XdpProfile,
    peer: &str,
) -> Result<(String, Duration)> {
    let name = xdp.name.as_str();
    let _build_dur = run_build(root, &xdp.build, &format!("xdp `{name}`"), peer)?;

    let path = abs_under_root(root, &xdp.artifact);
    if !path.is_file() {
        bail!(
            "xdp artifact not found after build: {} (xdp `{name}`)",
            path.display()
        );
    }
    let elf_name = xdp_name_from_file(&path)
        .with_context(|| format!("parse name from {}", path.display()))?;
    if elf_name != name {
        bail!(
            "xdp profile name `{name}` != ELF-derived name `{elf_name}` ({})",
            path.display()
        );
    }
    let sz = file_size(&path).unwrap_or(0);
    log_line(
        peer,
        &format!(
            "[xdp] `{name}`: {} ({})",
            path.display(),
            fmt_bytes(sz)
        ),
    );

    let t_flash = Instant::now();
    // Transient espflash bootloader errors after HAL↔XDP fw switches; retry once.
    let outcome = {
        const ATTEMPTS: usize = 2;
        let mut last = None;
        for attempt in 1..=ATTEMPTS {
            let g = lock(state)?;
            match g.handler.flash(FlashOp::Xdp {
                name: elf_name.clone(),
                path: path.display().to_string(),
            }) {
                Ok(o) => {
                    last = Some(Ok(o));
                    break;
                }
                Err(e) => {
                    let msg = e.to_string();
                    let retryable = msg.contains("bootloader returned an error");
                    if retryable && attempt < ATTEMPTS {
                        log_line(
                            peer,
                            &format!(
                                "[xdp] `{name}` bootloader error on attempt {attempt}/{ATTEMPTS}; retry"
                            ),
                        );
                        drop(g);
                        std::thread::sleep(Duration::from_millis(800));
                        last = Some(Err(e));
                        continue;
                    }
                    last = Some(Err(e));
                    break;
                }
            }
        }
        last
            .unwrap_or_else(|| Err(std::io::Error::other("flash xdp: no attempts")))
            .with_context(|| format!("flash xdp `{name}`"))?
    };
    let flash_dur = t_flash.elapsed();
    let summary = match outcome {
        FlashResult::Install(xpost_serial::InstallOutcome::Overwritten { offset }) => {
            format!(
                "overwritten name={name} offset={offset:#x} size={}",
                fmt_bytes(sz)
            )
        }
        FlashResult::Install(xpost_serial::InstallOutcome::Created { offset }) => format!(
            "created name={name} offset={offset:#x} size={}",
            fmt_bytes(sz)
        ),
        FlashResult::Done => format!("done name={name} size={}", fmt_bytes(sz)),
        FlashResult::List(_) => bail!("unexpected list result from flash xdp"),
    };
    Ok((summary, flash_dur))
}

fn run_build(root: &Path, build: &BuildSpec, label: &str, peer: &str) -> Result<Duration> {
    let cwd = abs_under_root(root, &build.cwd);
    let (prog, args) = build
        .command
        .split_first()
        .context(format!("{label}: empty build.command"))?;
    log_line(
        peer,
        &format!(
            "build {label}: {} {}  (cwd {})",
            prog,
            args.join(" "),
            cwd.display()
        ),
    );
    let t0 = Instant::now();
    let status = ProcCommand::new(prog)
        .args(args)
        .current_dir(&cwd)
        .status()
        .with_context(|| format!("spawn build for {label}"))?;
    let elapsed = t0.elapsed();
    if !status.success() {
        bail!(
            "build failed for {label} in {} (status={status}): {} {}",
            fmt_dur(elapsed),
            prog,
            args.join(" ")
        );
    }
    log_line(
        peer,
        &format!("build {label} ok in {}", fmt_dur(elapsed)),
    );
    Ok(elapsed)
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

/// Frozen HTTP map JSON (`GET /v1/get/map/{name}`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct MapJson {
    pub ok: bool,
    pub name: String,
    #[serde(rename = "type")]
    pub map_type: String,
    pub entries: Vec<MapKvJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cap: Option<u32>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MapKvJson {
    pub key: String,
    pub value: String,
}

/// App `GetMap` → decoded map JSON (HTTP contract).
pub fn get_map_json(state: &Mutex<AgentState>, name: &str, peer: &str) -> Result<MapJson> {
    let name = name.trim();
    if name.is_empty() {
        bail!("empty map name");
    }
    let blob = with_log(peer, "GetMap", name, true, state, || {
        let g = lock(state)?;
        match req(&g, &Command::GetMap { name })? {
            OkReply::GetMap { blob } => Ok(String::from_utf8_lossy(blob).into_owned()),
            _ => bail!("unexpected reply for get map"),
        }
    })?;
    decode_map_blob(name, blob.as_bytes())
}

fn decode_map_blob(name: &str, blob: &[u8]) -> Result<MapJson> {
    let text = std::str::from_utf8(blob).context("map blob is not UTF-8")?;
    let mut lines = text.lines().filter(|l| !l.is_empty());
    let header = lines.next().context("map blob missing header")?;
    let mut parts = header.split_whitespace();
    let kind = parts.next().context("map blob header kind")?;
    match kind {
        "array" => {
            let size: u32 = parts
                .next()
                .context("array header size")?
                .parse()
                .context("array size")?;
            let mut entries = Vec::with_capacity(size as usize);
            for line in lines {
                let mut kv = line.split_whitespace();
                let key = kv.next().context("array entry key")?.to_string();
                let value = kv.next().context("array entry value")?.to_string();
                entries.push(MapKvJson { key, value });
            }
            Ok(MapJson {
                ok: true,
                name: name.to_string(),
                map_type: "array".into(),
                entries,
                size: Some(size),
                cap: None,
            })
        }
        "hash" => {
            let size: u32 = parts
                .next()
                .context("hash header size")?
                .parse()
                .context("hash size")?;
            let cap: u32 = parts
                .next()
                .context("hash header cap")?
                .parse()
                .context("hash cap")?;
            let mut entries = Vec::with_capacity(size as usize);
            for line in lines {
                let mut kv = line.split_whitespace();
                let key = kv.next().context("hash entry key")?.to_string();
                let value = kv.next().context("hash entry value")?.to_string();
                entries.push(MapKvJson { key, value });
            }
            Ok(MapJson {
                ok: true,
                name: name.to_string(),
                map_type: "hash".into(),
                entries,
                size: Some(size),
                cap: Some(cap),
            })
        }
        other => bail!("unknown map blob kind `{other}`"),
    }
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
