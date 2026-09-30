//! Synchronous USB / probe operations shared by HTTP and MCP.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use xpost_serial::{Image, JUST_DROP_TABLE, XdpImage, xdp_name_from_file};

use crate::state::{AgentState, NOISY_LOG_EVERY, ProfileEntry};
use crate::util::{file_size, fmt_bytes, fmt_dur, log_line, truncate};
use crate::{probe_ack, probe_burst};

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

pub fn get_status() -> String {
    "idle".into()
}

const READY_NEEDLE: &str = "xpost ready";

fn clear_dut_ready(g: &mut AgentState) {
    g.dut_ready = false;
    g.dut_ready_line = None;
}

fn note_uart_lines(g: &mut AgentState, lines: &[String]) {
    for line in lines {
        if line.contains(READY_NEEDLE) {
            g.dut_ready = true;
            g.dut_ready_line = Some(line.clone());
        }
    }
}

/// Drain pending UART and report DUT boot readiness (`ready …` or `booting`).
pub fn dut_status(state: &Mutex<AgentState>) -> Result<String> {
    let mut g = state.lock().map_err(|_| anyhow::anyhow!("session lock"))?;
    let lines = g.session.drain_uart().context("drain_uart")?;
    note_uart_lines(&mut g, &lines);
    if g.dut_ready {
        let line = g.dut_ready_line.as_deref().unwrap_or(READY_NEEDLE);
        Ok(format!("ready {line}"))
    } else {
        Ok("booting".into())
    }
}

pub fn flash_profile(state: &Mutex<AgentState>, profile_id: &str, peer: &str) -> Result<String> {
    with_log(peer, "FlashProfile", &format!("profile=`{profile_id}`"), false, state, || {
        flash_profile_inner(state, profile_id, peer)
    })
}

fn flash_profile_inner(state: &Mutex<AgentState>, id: &str, peer: &str) -> Result<String> {
    let (root, entry) = {
        let g = state.lock().map_err(|_| anyhow::anyhow!("session lock"))?;
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
    let xdp_path = entry.xdp_elf.as_ref().map(|p| root.join(p));
    if let Some(ref xp) = xdp_path
        && !xp.is_file()
    {
        bail!("xdp elf not found: {}", xp.display());
    }
    let xdp_name_owned = match (&entry.xdp_name, &xdp_path) {
        (Some(n), _) => Some(n.clone()),
        (None, Some(xp)) => Some(
            xdp_name_from_file(xp)
                .with_context(|| format!("xdp name from {}", xp.display()))?,
        ),
        _ => None,
    };
    let xdp_sz = xdp_path
        .as_ref()
        .and_then(|p| file_size(p))
        .unwrap_or(0);

    log_line(
        peer,
        &format!(
            "[fw] profile `{id}`: {} ({})  [part] {}",
            firmware.display(),
            fmt_bytes(fw_sz),
            partitions.display(),
        ),
    );
    if let (Some(n), Some(p)) = (&xdp_name_owned, &xdp_path) {
        log_line(
            peer,
            &format!(
                "[xdp] profile `{id}`: name={n} {} ({})",
                p.display(),
                fmt_bytes(xdp_sz)
            ),
        );
    } else {
        log_line(peer, &format!("[xdp] profile `{id}`: (none)"));
    }

    let mut g = state.lock().map_err(|_| anyhow::anyhow!("session lock"))?;
    clear_dut_ready(&mut g);
    g.session.cold_table = JUST_DROP_TABLE;
    g.session.image_capacity = JUST_DROP_TABLE.image_capacity;
    let xdp = match (&xdp_name_owned, &xdp_path) {
        (Some(name), Some(path)) => Some(XdpImage {
            name: name.as_str(),
            path: path.as_path(),
        }),
        _ => None,
    };
    let img = Image {
        firmware: firmware.as_path(),
        partitions: partitions.as_path(),
        xdp,
    };
    let t_flash = Instant::now();
    log_line(
        peer,
        &format!(
            "[fw]+[xdp] profile `{id}`: USB program (cold_table={:#x}+{:#x}) — watch [fw]/[part]/[xdp]/[reset]/[uart]/[cmd] below",
            JUST_DROP_TABLE.base, JUST_DROP_TABLE.len
        ),
    );
    g.session
        .program(&img)
        .with_context(|| format!("program profile `{id}`"))?;
    let flash_dur = t_flash.elapsed();
    log_line(
        peer,
        &format!(
            "[reset] profile `{id}`: program+reset done in {} (build {})",
            fmt_dur(flash_dur),
            fmt_dur(build_dur)
        ),
    );
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
    let status = Command::new(prog)
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

pub fn program(state: &Mutex<AgentState>, elf: &str, peer: &str) -> Result<String> {
    with_log(peer, "Program", &format!("elf={elf}"), false, state, || {
        let path = PathBuf::from(elf.trim());
        if !path.is_file() {
            bail!("elf not found: {}", path.display());
        }
        let name = xdp_name_from_file(&path)
            .with_context(|| format!("parse name from {}", path.display()))?;
        let sz = file_size(&path).unwrap_or(0);
        let mut g = state.lock().map_err(|_| anyhow::anyhow!("session lock"))?;
        clear_dut_ready(&mut g);
        g.session.cold_table = JUST_DROP_TABLE;
        g.session.image_capacity = JUST_DROP_TABLE.image_capacity;
        let outcome = g
            .session
            .install_xdp(&name, &path)
            .with_context(|| format!("install_xdp `{name}`"))?;
        g.session
            .reset_to_app()
            .context("reset_to_app after program")?;
        Ok(match outcome {
            xpost_serial::InstallOutcome::Overwritten { offset } => {
                format!(
                    "overwritten name={name} offset={offset:#x} size={}",
                    fmt_bytes(sz)
                )
            }
            xpost_serial::InstallOutcome::Created { offset } => {
                format!(
                    "created name={name} offset={offset:#x} size={}",
                    fmt_bytes(sz)
                )
            }
        })
    })
}

pub fn clear_xdp(state: &Mutex<AgentState>, peer: &str) -> Result<String> {
    with_log(peer, "Clear", "", false, state, || {
        let mut g = state.lock().map_err(|_| anyhow::anyhow!("session lock"))?;
        clear_dut_ready(&mut g);
        g.session.cold_table = JUST_DROP_TABLE;
        g.session.clear_xdp().context("clear_xdp")?;
        g.session.reset_to_app().context("reset_to_app after clear")?;
        Ok("cleared".into())
    })
}

pub fn list_xdp(state: &Mutex<AgentState>, peer: &str) -> Result<String> {
    with_log(peer, "List", "", false, state, || {
        let mut g = state.lock().map_err(|_| anyhow::anyhow!("session lock"))?;
        g.session.cold_table = JUST_DROP_TABLE;
        let entries = g.session.list_xdp().context("list_xdp")?;
        if entries.is_empty() {
            Ok("(empty cold table)".into())
        } else {
            let mut lines = Vec::with_capacity(entries.len());
            for e in &entries {
                lines.push(format!(
                    "name={} offset={:#x} len={:#x}",
                    e.name, e.offset, e.len
                ));
            }
            Ok(lines.join("\n"))
        }
    })
}

pub fn counters(state: &Mutex<AgentState>, peer: &str) -> Result<String> {
    with_log(peer, "Counters", "", true, state, || {
        let mut g = state.lock().map_err(|_| anyhow::anyhow!("session lock"))?;
        let (_hdr, payload) = g
            .session
            .request_report_timeout(Duration::from_secs(5))
            .context("request report for counters")?;
        let report = xpost_serial::decode_perf_report(&payload).context("decode report")?;
        Ok(xpost_serial::format_counters_ok(&report.counters))
    })
}

pub fn request(state: &Mutex<AgentState>, cmd: &str, peer: &str) -> Result<String> {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        bail!("empty request");
    }
    let is_report = cmd.eq_ignore_ascii_case("report") || cmd.eq_ignore_ascii_case("report perf");
    let noisy = cmd.starts_with("rxcnt") || is_report;
    with_log(peer, "Request", &format!("`{cmd}`"), noisy, state, || {
        let mut g = state.lock().map_err(|_| anyhow::anyhow!("session lock"))?;
        if is_report {
            let (hdr, payload) = g
                .session
                .request_report_timeout(Duration::from_secs(5))
                .context("request report")?;
            let b64 = base64_encode(&payload);
            Ok(format!("{hdr} b64={b64}"))
        } else {
            g.session
                .request_timeout(cmd, Duration::from_secs(5))
                .with_context(|| format!("request `{cmd}`"))
        }
    })
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

/// Return sink transcript lines whose host timestamp falls in `[since_unix_ms, until_unix_ms]`.
///
/// Also drains pending UART into the sink first so late `tsflow` lines are visible.
pub fn serial_window(
    state: &Mutex<AgentState>,
    since_unix_ms: u64,
    until_unix_ms: u64,
    peer: &str,
) -> Result<String> {
    use std::time::{Duration, UNIX_EPOCH};
    with_log(
        peer,
        "SerialWindow",
        &format!("ms={since_unix_ms}..{until_unix_ms}"),
        false,
        state,
        || {
            let mut g = state.lock().map_err(|_| anyhow::anyhow!("session lock"))?;
            let _ = g.session.drain_uart();
            let since = UNIX_EPOCH + Duration::from_millis(since_unix_ms);
            let until = UNIX_EPOCH + Duration::from_millis(until_unix_ms);
            let lines = g.session.serial_window(since, until);
            Ok(format!(
                "OK serial_window n={} since_ms={since_unix_ms} until_ms={until_unix_ms}\n{}",
                lines.len(),
                lines.join("\n")
            ))
        },
    )
}

pub fn probe(esp: &str, port: u16, peer: &str) -> Result<String> {
    let t0 = Instant::now();
    log_line(peer, &format!("→ Probe {esp}:{port}"));
    match probe_ack(esp, port, Duration::from_secs(2)) {
        Ok(got) => {
            log_line(
                peer,
                &format!("← Probe ok in {} — {got}", fmt_dur(t0.elapsed())),
            );
            Ok(got.into())
        }
        Err(e) => {
            log_line(
                peer,
                &format!("✗ Probe failed in {}: {e:#}", fmt_dur(t0.elapsed())),
            );
            Err(e)
        }
    }
}

pub fn probe_burst_op(esp: &str, port: u16, count: u32, peer: &str) -> Result<String> {
    if count == 0 || count > 100_000 {
        bail!("burst count out of range: {count}");
    }
    let t0 = Instant::now();
    log_line(peer, &format!("→ ProbeBurst {esp}:{port}:{count}"));
    match probe_burst(esp, port, count) {
        Ok(sent) => {
            let msg = format!("sent={sent}");
            log_line(
                peer,
                &format!("← ProbeBurst ok in {} — {msg}", fmt_dur(t0.elapsed())),
            );
            Ok(msg)
        }
        Err(e) => {
            log_line(
                peer,
                &format!("✗ ProbeBurst failed in {}: {e:#}", fmt_dur(t0.elapsed())),
            );
            Err(e)
        }
    }
}
