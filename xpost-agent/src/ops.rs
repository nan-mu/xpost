//! Synchronous USB operations used by HTTP.

use std::path::Path;
use std::process::Command as ProcCommand;
use std::sync::Mutex;
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use xpost_serial::client::flash::{InstallOutcome, XdpEntry, XdpTable, plan_install};
use xpost_serial::client::{Command, FirmwareExpectation, FlashOp, FlashResult, OkReply};
use xpost_serial::command::{
    EXEC_BATCH_BYTES, EXEC_BATCH_READ_MAX_BYTES, EXEC_BATCH_WRITE_MAX_BYTES, PerfVariant,
};

use crate::deployment::{self, PreparedImage, PreparedInfo, Target, firmware_snapshot};
use crate::profiles::{BuildSpec, XdpProfile, abs_under_root};
use crate::release::format_sha256;
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

/// Unix seconds for a timestamp the UART reader took when it read the reply
/// line. Passing that instant keeps `agent_rx` at the moment the MCU reply
/// arrived instead of the later moment this process formats it.
fn agent_rx_unix(at: std::time::SystemTime) -> f64 {
    at.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn with_log<T: AsRef<str>>(
    peer: &str,
    name: &str,
    summary: &str,
    noisy: bool,
    state: &Mutex<AgentState>,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
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
                        truncate(msg.as_ref(), 120)
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
    g.handler.request(cmd).map_err(|e| anyhow::anyhow!("{e}"))
}

/// [`req`] plus the instant the UART reader read the reply line.
fn req_at<'a>(
    g: &'a AgentState,
    cmd: &Command<'_>,
) -> Result<(OkReply<'a>, std::time::SystemTime)> {
    g.handler
        .request_at(cmd)
        .map_err(|e| anyhow::anyhow!("{e}"))
}

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

pub struct OpReply {
    pub message: String,
    pub flash: Option<FlashIdentity>,
}

impl From<String> for OpReply {
    fn from(message: String) -> Self {
        Self {
            message,
            flash: None,
        }
    }
}

impl AsRef<str> for OpReply {
    fn as_ref(&self) -> &str {
        &self.message
    }
}

#[derive(Serialize)]
pub struct FlashIdentity {
    firmware_profile: String,
    firmware_sha256: String,
    containers: Vec<PreparedInfo>,
}

pub fn flash_fw_checked(
    state: &Mutex<AgentState>,
    name: &str,
    expected_sha256: Option<&str>,
    peer: &str,
) -> Result<OpReply> {
    with_log(
        peer,
        "FlashFw",
        &format!("name=`{name}`"),
        false,
        state,
        || flash_fw_inner(state, name, expected_sha256, peer),
    )
}

fn flash_fw_inner(
    state: &Mutex<AgentState>,
    name: &str,
    expected_sha256: Option<&str>,
    peer: &str,
) -> Result<OpReply> {
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

    let exec_hal = crate::preflight::hal_exec_task(name).is_some();
    if exec_hal {
        anyhow::ensure!(
            linked_xdps.is_empty(),
            "EXB2 HAL firmware cannot co-flash XDP"
        );
    }
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

    let target = if linked_xdps.is_empty() {
        None
    } else {
        Some(Target::read(&root, &fw)?)
    };
    let snapshot = match &target {
        Some(target) => target.snapshot.clone(),
        None => firmware_snapshot(&firmware, &partitions)?,
    };
    if exec_hal {
        crate::preflight::validate_hal(&fw, &snapshot.elf, false)?;
    }
    let firmware_sha = snapshot.sha256;
    if let Some(expected) = expected_sha256 {
        let actual = format_sha256(&firmware_sha);
        anyhow::ensure!(
            actual == expected,
            "Firmware ELF for `{name}` changed after preflight: expected {expected}, got {actual}"
        );
    }
    let t_flash = Instant::now();
    let mut cold_entries = Vec::new();
    let installed = prepare_and_flash(
        &linked_xdps,
        |profile| {
            let target = target.as_ref().context("missing co-flash target")?;
            let image = deployment::prepare(&root, target, profile)?;
            let len = u32::try_from(image.bytes.len())?;
            let plan = plan_install(&cold_entries, &profile.name, len, target.table.len)
                .with_context(|| format!("plan cold installation for `{}`", profile.name))?;
            if let InstallOutcome::Created { offset } = plan {
                cold_entries.push(XdpEntry {
                    name: profile.name.clone(),
                    offset,
                    len,
                });
            }
            Ok(image)
        },
        || {
            let g = lock(state)?;
            g.handler
                .flash(FlashOp::Fw {
                    snapshot: snapshot.clone(),
                })
                .with_context(|| format!("flash fw `{name}`"))?;
            Ok(g)
        },
        |g| {
            log_line(peer, "clear xdp table before co-flash");
            g.handler
                .flash_with_table(
                    FlashOp::Clear { names: Vec::new() },
                    target.as_ref().map(|t| t.table),
                )
                .context("clear xdp before co-flash")?;
            Ok(())
        },
        |g, image| {
            let message = flash_one_xdp(
                g,
                &image,
                &snapshot,
                target.as_ref().unwrap().table,
                false,
                peer,
            )?;
            Ok((message, image.info))
        },
    )?;
    let (xdp_parts, containers): (Vec<_>, Vec<_>) = installed.into_iter().unzip();
    let flash_dur = t_flash.elapsed();

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
    Ok(OpReply {
        message: msg,
        flash: Some(FlashIdentity {
            firmware_profile: name.to_owned(),
            firmware_sha256: format_sha256(&firmware_sha),
            containers,
        }),
    })
}

#[derive(Serialize)]
pub struct InstalledDeployment {
    pub firmware_matches: bool,
    pub container_sha256: Option<String>,
}

pub fn attest_deployment(
    state: &Mutex<AgentState>,
    firmware: &str,
    expected_sha256: &str,
) -> Result<InstalledDeployment> {
    let g = lock(state)?;
    let profile = g
        .profiles
        .fw(firmware)
        .with_context(|| format!("unknown firmware {firmware}"))?;
    let target = Target::read(&g.root, profile)?;
    anyhow::ensure!(
        format_sha256(&target.snapshot.sha256) == expected_sha256,
        "Firmware ELF changed since preflight"
    );
    let firmware_only = crate::preflight::hal_exec_task(firmware).is_some();
    if firmware_only {
        crate::preflight::validate_hal(profile, &target.snapshot.elf, false)?;
    }
    match g.handler.flash_with_table(
        FlashOp::Attest {
            expectation: target.snapshot,
            firmware_only,
        },
        Some(target.table),
    )? {
        FlashResult::Attested {
            firmware_matches,
            image_sha256,
        } => Ok(InstalledDeployment {
            firmware_matches,
            container_sha256: image_sha256.as_ref().map(format_sha256),
        }),
        _ => bail!("unexpected deployment attestation result"),
    }
}

/// A verified network or exec target from the experiment plan.
pub struct XdpTarget<'a> {
    pub firmware: &'a str,
    pub sha256: &'a str,
}

pub fn flash_xdp_checked(
    state: &Mutex<AgentState>,
    name: &str,
    slot: Option<u8>,
    planned: Option<XdpTarget<'_>>,
    peer: &str,
) -> Result<OpReply> {
    if let Some(slot) = slot {
        anyhow::ensure!(
            planned.is_none(),
            "firmware binding applies to cold installs"
        );
        return flash_xdp_slot(state, name, slot, peer).map(OpReply::from);
    }
    with_log(
        peer,
        "FlashXdp",
        &format!("name=`{name}`"),
        false,
        state,
        || {
            let (root, xdp, fw) = {
                let g = lock(state)?;
                let xdp = g
                    .profiles
                    .xdp(name)
                    .cloned()
                    .with_context(|| format!("unknown xdp `{name}`"))?;
                let firmware = planned
                    .as_ref()
                    .map_or(xdp.fw.as_str(), |target| target.firmware);
                let fw = g
                    .profiles
                    .fw(firmware)
                    .cloned()
                    .with_context(|| format!("unknown Firmware ELF profile `{firmware}`"))?;
                (g.root.clone(), xdp, fw)
            };
            let target = Target::read(&root, &fw).with_context(|| {
                format!(
                    "libbpf-mcu {} program {} mode {} target {}",
                    crate::release::TAG,
                    xdp.program,
                    xdp.mode.as_str(),
                    fw.name
                )
            })?;
            if let Some(planned) = &planned {
                let actual = format_sha256(&target.snapshot.sha256);
                anyhow::ensure!(
                    actual == planned.sha256,
                    "Firmware ELF for `{}` changed after preflight: expected {}, got {actual}",
                    fw.name,
                    planned.sha256
                );
            }
            let image = deployment::prepare(&root, &target, &xdp)?;
            let g = lock(state)?;
            let message = flash_one_xdp(
                &g,
                &image,
                &target.snapshot,
                target.table,
                planned.is_some(),
                peer,
            )?;
            Ok(OpReply {
                message,
                flash: Some(FlashIdentity {
                    firmware_profile: fw.name,
                    firmware_sha256: format_sha256(&target.snapshot.sha256),
                    containers: vec![image.info],
                }),
            })
        },
    )
}

fn prepare_and_flash<G, I, T>(
    xdps: &[XdpProfile],
    mut resolve: impl FnMut(&XdpProfile) -> Result<I>,
    firmware: impl FnOnce() -> Result<G>,
    clear: impl FnOnce(&G) -> Result<()>,
    mut install: impl FnMut(&G, I) -> Result<T>,
) -> Result<Vec<T>> {
    let images = xdps.iter().map(&mut resolve).collect::<Result<Vec<_>>>()?;
    let session = firmware()?;
    if !images.is_empty() {
        clear(&session)?;
    }
    images
        .into_iter()
        .map(|image| install(&session, image))
        .collect()
}

fn cold_install_op(
    image: &PreparedImage,
    firmware: &FirmwareExpectation,
    replace_single: bool,
) -> Result<FlashOp> {
    if image.info.firmware_sha256 != format_sha256(&firmware.sha256) {
        bail!(
            "xdp `{}`: firmware snapshot does not match the prepared image",
            image.info.profile
        );
    }
    Ok(FlashOp::Xdp {
        name: image.info.profile.clone(),
        bytes: image.bytes.clone(),
        expectation: firmware.clone(),
        replace_single,
    })
}

/// Install a verified image from the retained bytes.
fn flash_one_xdp(
    state: &AgentState,
    image: &PreparedImage,
    snapshot: &FirmwareExpectation,
    table: XdpTable,
    replace_single: bool,
    peer: &str,
) -> Result<String> {
    let name = &image.info.profile;
    let path = &image.info.deployment.member;
    let sz = image.bytes.len() as u64;
    log_line(peer, &format!("[xdp] `{name}`: {path} ({})", fmt_bytes(sz)));

    let op = cold_install_op(image, snapshot, replace_single)?;
    // Transient espflash bootloader errors after HAL↔XDP fw switches; retry once.
    let outcome = {
        const ATTEMPTS: usize = 2;
        let mut last = None;
        for attempt in 1..=ATTEMPTS {
            match state.handler.flash_with_table(op.clone(), Some(table)) {
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
                        std::thread::sleep(Duration::from_millis(800));
                        last = Some(Err(e));
                        continue;
                    }
                    last = Some(Err(e));
                    break;
                }
            }
        }
        last.unwrap_or_else(|| Err(std::io::Error::other("flash xdp: no attempts")))
            .with_context(|| format!("flash xdp `{name}`"))?
    };
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
        FlashResult::List(_) | FlashResult::Attested { .. } => {
            bail!("unexpected result from flash xdp")
        }
    };
    Ok(summary)
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
    // Firmware builds write under their cwd. An inherited CARGO_TARGET_DIR
    // would move the ELF away from the configured firmware artifact path.
    let status = ProcCommand::new(prog)
        .args(args)
        .current_dir(&cwd)
        .env_remove("CARGO_TARGET_DIR")
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
    log_line(peer, &format!("build {label} ok in {}", fmt_dur(elapsed)));
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
            FlashResult::Done | FlashResult::Install(_) | FlashResult::Attested { .. } => {
                bail!("unexpected flash result from get xdp")
            }
        }
    })
}

pub fn enable_xdp(state: &Mutex<AgentState>, peer: &str) -> Result<String> {
    with_log(peer, "EnableXdp", "", false, state, || {
        let g = lock(state)?;
        let (reply, at) = req_at(&g, &Command::EnableXdp)?;
        let mcu_us = match reply {
            OkReply::EnableXdpStamped { mcu_us } => mcu_us,
            _ => 0,
        };
        let agent_rx = agent_rx_unix(at);
        Ok(format!("OK enable xdp {mcu_us} agent_rx={agent_rx:.6}"))
    })
}

pub fn disable_xdp(state: &Mutex<AgentState>, peer: &str) -> Result<String> {
    with_log(peer, "DisableXdp", "", false, state, || {
        let g = lock(state)?;
        let (_, at) = req_at(&g, &Command::DisableXdp)?;
        let agent_rx = agent_rx_unix(at);
        Ok(format!("OK disable xdp agent_rx={agent_rx:.6}"))
    })
}

/// Copy an installed cold container into a hot slot.
pub fn flash_xdp_slot(
    state: &Mutex<AgentState>,
    name: &str,
    slot: u8,
    peer: &str,
) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("empty xdp name");
    }
    with_log(peer, "XdpFlash", name, false, state, || {
        let g = lock(state)?;
        match req_at(&g, &Command::XdpFlash { name, slot })? {
            (
                OkReply::XdpFlash {
                    slot,
                    generation,
                    mcu_us,
                },
                at,
            ) => {
                let agent_rx = agent_rx_unix(at);
                Ok(format!(
                    "OK xdp flash {slot} {generation} {mcu_us} agent_rx={agent_rx:.6}"
                ))
            }
            _ => bail!("unexpected xdp flash reply"),
        }
    })
}

pub fn switch_xdp(
    state: &Mutex<AgentState>,
    slot: u8,
    generation: u32,
    pass: bool,
    peer: &str,
) -> Result<String> {
    with_log(peer, "XdpSwitch", "", false, state, || {
        let g = lock(state)?;
        let reply = g
            .handler
            .request_at(&Command::XdpSwitch {
                slot,
                generation,
                pass,
            })
            .map_err(|error| match error {
                AgentError::Server(_) | AgentError::Syntax(_) => anyhow::anyhow!("{error}"),
                _ => anyhow::anyhow!("{error}").context("XDP switch outcome unknown"),
            })?;
        match reply {
            (
                OkReply::XdpSwitch {
                    ok,
                    rollback,
                    severe,
                    from_slot,
                    to_slot,
                    generation,
                    quiesce_drop,
                    quiesce_pass,
                    mcu_us,
                },
                at,
            ) => {
                let agent_rx = agent_rx_unix(at);
                let reply = format!(
                    "OK xdp switch {ok} {rollback} {severe} {from_slot} {to_slot} {generation} {quiesce_drop} {quiesce_pass} {mcu_us} agent_rx={agent_rx:.6}"
                );
                anyhow::ensure!(ok == 1, "{reply}");
                Ok(reply)
            }
            _ => bail!("unexpected xdp switch reply"),
        }
    })
}

pub fn xdp_status(state: &Mutex<AgentState>, peer: &str) -> Result<String> {
    with_log(peer, "GetXdpStatus", "", false, state, || {
        let g = lock(state)?;
        match req(&g, &Command::GetXdpStatus)? {
            OkReply::GetXdpStatus {
                boot_id,
                active_slot,
                staged_slot,
                generation,
                updating,
                severe,
                quiesce_drop,
                quiesce_pass,
                last_error,
            } => Ok(format!(
                "OK get xdp {boot_id} {active_slot} {staged_slot} {generation} {updating} {severe} {quiesce_drop} {quiesce_pass} {last_error}"
            )),
            _ => bail!("unexpected get xdp reply"),
        }
    })
}

pub fn enable_exec(state: &Mutex<AgentState>, peer: &str) -> Result<String> {
    with_log(peer, "EnableExec", "", false, state, || {
        let g = lock(state)?;
        req(&g, &Command::EnableExec)?;
        Ok("OK enable exec".into())
    })
}

pub fn disable_exec(state: &Mutex<AgentState>, peer: &str) -> Result<String> {
    with_log(peer, "DisableExec", "", false, state, || {
        let g = lock(state)?;
        req(&g, &Command::DisableExec)?;
        Ok("OK disable exec".into())
    })
}

#[derive(Serialize)]
pub struct BatchReply {
    boot_id: u32,
    begin_tick: u64,
    end_tick: u64,
    execution_return: u32,
    output: String,
}

/// Upload, invoke, and read one EXB2 buffer under the same device lock.
pub fn exec_batch(state: &Mutex<AgentState>, input_hex: &str) -> Result<BatchReply> {
    let input = decode_hex::<EXEC_BATCH_BYTES>(input_hex).context("decode EXB2 input")?;
    anyhow::ensure!(
        input.starts_with(b"EXB2\x02\0\x40\0"),
        "invalid EXB2 header"
    );
    let g = lock(state)?;
    anyhow::ensure!(
        matches!(req(&g, &Command::ExecBatchReset)?, OkReply::ExecBatchReset),
        "unexpected EXB2 reset reply"
    );
    for (chunk, data) in input_hex
        .as_bytes()
        .chunks(EXEC_BATCH_WRITE_MAX_BYTES * 2)
        .enumerate()
    {
        let offset = (chunk * EXEC_BATCH_WRITE_MAX_BYTES) as u32;
        let reply = req(&g, &Command::ExecBatchWrite { offset, data })
            .with_context(|| format!("upload EXB2 offset {offset}"))?;
        anyhow::ensure!(
            matches!(reply, OkReply::ExecBatchWrite),
            "unexpected EXB2 write reply"
        );
    }
    let OkReply::ExecBatchInvoke {
        boot_id,
        begin_tick,
        end_tick,
        action,
    } = req(&g, &Command::ExecBatchInvoke).context("EXB2 invocation outcome unknown")?
    else {
        bail!("unexpected EXB2 invoke reply");
    };
    anyhow::ensure!(
        boot_id != 0 && begin_tick <= end_tick,
        "invalid EXB2 execution clock"
    );
    let mut output = Vec::with_capacity(EXEC_BATCH_BYTES);
    while output.len() < EXEC_BATCH_BYTES {
        let count = EXEC_BATCH_READ_MAX_BYTES.min(EXEC_BATCH_BYTES - output.len());
        let offset = output.len() as u32;
        let OkReply::ExecBatchRead { blob } = req(
            &g,
            &Command::ExecBatchRead {
                offset,
                max_bytes: count as u32,
            },
        )
        .with_context(|| format!("read EXB2 result at offset {offset} after invoke"))?
        else {
            bail!("unexpected EXB2 read reply");
        };
        anyhow::ensure!(blob.len() == count, "short EXB2 result at offset {offset}");
        output.extend_from_slice(blob);
    }
    Ok(BatchReply {
        boot_id,
        begin_tick,
        end_tick,
        execution_return: action,
        output: base64_encode(&output),
    })
}

/// Host-only wait for unsolicited `OK finish` (eBPF printk), via [`Command::GetFinish`].
pub fn get_finish(state: &Mutex<AgentState>, peer: &str) -> Result<String> {
    with_log(peer, "GetFinish", "", false, state, || {
        let g = lock(state)?;
        req(&g, &Command::GetFinish)?;
        Ok("OK finish".into())
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
                req(&g, &Command::UpdateMap { name, key, value })?;
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

pub fn perf_prepare(
    state: &Mutex<AgentState>,
    uuid_hex: &str,
    variant: &str,
    profile_hex: &str,
    peer: &str,
) -> Result<String> {
    let uuid = decode_hex(uuid_hex)?;
    let profile = decode_hex(profile_hex)?;
    let kind = perf_kind(variant)?;
    let summary = format!("variant={variant}");
    with_log(peer, "PerfPrepare", &summary, false, state, || {
        let g = lock(state)?;
        format_perf_session(&req(
            &g,
            &Command::PerfPrepare {
                uuid,
                kind,
                profile,
            },
        )?)
    })
}

pub fn perf_arm(state: &Mutex<AgentState>, epoch: u32, peer: &str) -> Result<String> {
    with_log(
        peer,
        "PerfArm",
        &format!("epoch={epoch}"),
        false,
        state,
        || {
            let g = lock(state)?;
            format_perf_session(&req(&g, &Command::PerfArm { epoch })?)
        },
    )
}

pub fn perf_freeze(state: &Mutex<AgentState>, epoch: u32, peer: &str) -> Result<String> {
    with_log(
        peer,
        "PerfFreeze",
        &format!("epoch={epoch}"),
        false,
        state,
        || {
            let g = lock(state)?;
            format_perf_session(&req(&g, &Command::PerfFreeze { epoch })?)
        },
    )
}

pub fn perf_read(
    state: &Mutex<AgentState>,
    epoch: u32,
    offset: u32,
    max_bytes: u32,
    peer: &str,
) -> Result<String> {
    with_log(
        peer,
        "PerfRead",
        &format!("epoch={epoch} offset={offset}"),
        false,
        state,
        || {
            let g = lock(state)?;
            format_perf_session(&req(
                &g,
                &Command::PerfRead {
                    epoch,
                    offset,
                    max_bytes,
                },
            )?)
        },
    )
}

pub fn perf_release(state: &Mutex<AgentState>, epoch: u32, peer: &str) -> Result<String> {
    with_log(
        peer,
        "PerfRelease",
        &format!("epoch={epoch}"),
        false,
        state,
        || {
            let g = lock(state)?;
            format_perf_session(&req(&g, &Command::PerfRelease { epoch })?)
        },
    )
}

fn format_perf_session(reply: &OkReply<'_>) -> Result<String> {
    match reply {
        OkReply::PerfPrepare { epoch, boot_id } => {
            Ok(format!("OK perf prepare epoch={epoch} boot_id={boot_id}"))
        }
        OkReply::PerfArm { tick } => Ok(format!("OK perf arm tick={tick}")),
        OkReply::PerfFreeze {
            tick,
            flags,
            len,
            digest,
        } => Ok(format!(
            "OK perf freeze tick={tick} flags={flags} len={len} digest={digest}"
        )),
        OkReply::PerfRead {
            total,
            digest,
            blob,
        } => Ok(format!(
            "OK perf read total={total} digest={digest} n={} b64={}",
            blob.len(),
            base64_encode(blob)
        )),
        OkReply::PerfRelease => Ok("OK perf release".into()),
        _ => bail!("unexpected perf session reply"),
    }
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
            let _ = g.handler.request(&Command::Ping);
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
    with_log(
        peer,
        "Run",
        &format!("`{owned}`"),
        false,
        state,
        move || {
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
                ParsedCmd::EnableExec => {
                    req(&g, &Command::EnableExec)?;
                    Ok("OK enable exec".into())
                }
                ParsedCmd::DisableExec => {
                    req(&g, &Command::DisableExec)?;
                    Ok("OK disable exec".into())
                }
                ParsedCmd::ClearMap { name } => {
                    req(&g, &Command::ClearMap { name })?;
                    Ok("OK clear map".into())
                }
                ParsedCmd::GetMap { name } => match req(&g, &Command::GetMap { name })? {
                    OkReply::GetMap { blob } => Ok(format!(
                        "OK get map {} b64={}",
                        blob.len(),
                        base64_encode(blob)
                    )),
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
                    OkReply::GetPerf { blob } => Ok(format!(
                        "OK get perf {} b64={}",
                        blob.len(),
                        base64_encode(blob)
                    )),
                    _ => bail!("unexpected get perf reply"),
                },
                ParsedCmd::PerfPrepare {
                    uuid,
                    kind,
                    profile,
                } => format_perf_session(&req(
                    &g,
                    &Command::PerfPrepare {
                        uuid: *uuid,
                        kind: *kind,
                        profile: *profile,
                    },
                )?),
                ParsedCmd::PerfArm { epoch } => {
                    format_perf_session(&req(&g, &Command::PerfArm { epoch: *epoch })?)
                }
                ParsedCmd::PerfFreeze { epoch } => {
                    format_perf_session(&req(&g, &Command::PerfFreeze { epoch: *epoch })?)
                }
                ParsedCmd::PerfRead {
                    epoch,
                    offset,
                    max_bytes,
                } => format_perf_session(&req(
                    &g,
                    &Command::PerfRead {
                        epoch: *epoch,
                        offset: *offset,
                        max_bytes: *max_bytes,
                    },
                )?),
                ParsedCmd::PerfRelease { epoch } => {
                    format_perf_session(&req(&g, &Command::PerfRelease { epoch: *epoch })?)
                }
            }
        },
    )
}

enum ParsedCmd<'a> {
    Ping,
    EnableXdp,
    DisableXdp,
    EnableExec,
    DisableExec,
    ClearMap {
        name: &'a str,
    },
    GetMap {
        name: &'a str,
    },
    UpdateMap {
        name: &'a str,
        key: &'a str,
        value: &'a str,
    },
    ClearPerf,
    GetPerf,
    PerfPrepare {
        uuid: [u8; 16],
        kind: PerfVariant,
        profile: [u8; 32],
    },
    PerfArm {
        epoch: u32,
    },
    PerfFreeze {
        epoch: u32,
    },
    PerfRead {
        epoch: u32,
        offset: u32,
        max_bytes: u32,
    },
    PerfRelease {
        epoch: u32,
    },
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
    if eq(verb, "enable") && eq(target, "exec") {
        if parts.next().is_some() {
            bail!("ERR bad-arity");
        }
        return Ok(ParsedCmd::EnableExec);
    }
    if eq(verb, "disable") && eq(target, "exec") {
        if parts.next().is_some() {
            bail!("ERR bad-arity");
        }
        return Ok(ParsedCmd::DisableExec);
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
    if eq(verb, "perf") && eq(target, "prepare") {
        let uuid = parts.next().context("ERR bad-arity")?;
        let kind = parts.next().context("ERR bad-arity")?;
        let profile = parts.next().context("ERR bad-arity")?;
        if parts.next().is_some() {
            bail!("ERR bad-arity");
        }
        return Ok(ParsedCmd::PerfPrepare {
            uuid: decode_hex(uuid)?,
            kind: perf_kind(kind)?,
            profile: decode_hex(profile)?,
        });
    }
    if eq(verb, "perf") && eq(target, "arm") {
        let epoch = parts.next().context("ERR bad-arity")?;
        if parts.next().is_some() {
            bail!("ERR bad-arity");
        }
        return Ok(ParsedCmd::PerfArm {
            epoch: parse_u32_token(epoch)?,
        });
    }
    if eq(verb, "perf") && eq(target, "freeze") {
        let epoch = parts.next().context("ERR bad-arity")?;
        if parts.next().is_some() {
            bail!("ERR bad-arity");
        }
        return Ok(ParsedCmd::PerfFreeze {
            epoch: parse_u32_token(epoch)?,
        });
    }
    if eq(verb, "perf") && eq(target, "read") {
        let epoch = parts.next().context("ERR bad-arity")?;
        let offset = parts.next().context("ERR bad-arity")?;
        let max_bytes = parts.next().context("ERR bad-arity")?;
        if parts.next().is_some() {
            bail!("ERR bad-arity");
        }
        return Ok(ParsedCmd::PerfRead {
            epoch: parse_u32_token(epoch)?,
            offset: parse_u32_token(offset)?,
            max_bytes: parse_u32_token(max_bytes)?,
        });
    }
    if eq(verb, "perf") && eq(target, "release") {
        let epoch = parts.next().context("ERR bad-arity")?;
        if parts.next().is_some() {
            bail!("ERR bad-arity");
        }
        return Ok(ParsedCmd::PerfRelease {
            epoch: parse_u32_token(epoch)?,
        });
    }
    bail!("ERR unknown")
}

fn decode_hex<const N: usize>(text: &str) -> Result<[u8; N]> {
    let bytes = text.as_bytes();
    if bytes.len() != N * 2 {
        bail!("ERR bad-arity");
    }
    let mut out = [0u8; N];
    let mut i = 0;
    while i < N {
        let hi = hex_nibble(bytes[i * 2])?;
        let lo = hex_nibble(bytes[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
        i += 1;
    }
    Ok(out)
}

fn hex_nibble(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("ERR bad-arity"),
    }
}

fn perf_kind(text: &str) -> Result<PerfVariant> {
    if text.eq_ignore_ascii_case("metrics") {
        Ok(PerfVariant::Metrics)
    } else if text.eq_ignore_ascii_case("trace") {
        Ok(PerfVariant::Trace)
    } else {
        bail!("ERR bad-arity")
    }
}

fn parse_u32_token(text: &str) -> Result<u32> {
    if text.is_empty() || text.len() > 10 || !text.bytes().all(|b| b.is_ascii_digit()) {
        bail!("ERR bad-arity");
    }
    text.parse().map_err(|_| anyhow::anyhow!("ERR bad-arity"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn xdp(name: &str) -> XdpProfile {
        XdpProfile {
            name: name.into(),
            fw: "owner".into(),
            program: name.into(),
            mode: crate::release::Mode::Aot,
            slot: 0,
            target: None,
        }
    }

    #[test]
    fn preparation_guards_all_device_writes_and_preserves_order() {
        use std::cell::RefCell;
        let xdps = [xdp("first"), xdp("second")];
        let writes = RefCell::new(Vec::new());
        let result = prepare_and_flash(
            &xdps,
            |x| {
                if x.name == "second" {
                    bail!("resolver failure");
                }
                Ok(x.name.clone())
            },
            || {
                writes.borrow_mut().push("fw".to_string());
                Ok(())
            },
            |_| {
                writes.borrow_mut().push("clear".to_string());
                Ok(())
            },
            |_, x| {
                writes.borrow_mut().push(x.clone());
                Ok(x.clone())
            },
        );
        assert!(result.is_err());
        assert!(writes.borrow().is_empty());

        prepare_and_flash(
            &xdps,
            |x| Ok(x.name.clone()),
            || {
                writes.borrow_mut().push("fw".to_string());
                Ok(())
            },
            |_| {
                writes.borrow_mut().push("clear".to_string());
                Ok(())
            },
            |_, x| {
                writes.borrow_mut().push(x.clone());
                Ok(x.clone())
            },
        )
        .unwrap();
        assert_eq!(*writes.borrow(), ["fw", "clear", "first", "second"]);
    }

    #[test]
    fn firmware_flash_failure_prevents_clear_and_install() {
        let result = prepare_and_flash::<(), _, _>(
            &[xdp("first")],
            |x| Ok(x.name.clone()),
            || bail!("firmware flash failed"),
            |_| panic!("clear after failed firmware flash"),
            |_, _| -> Result<String> { panic!("install after failed firmware flash") },
        );
        assert!(result.is_err());
    }

    #[test]
    fn hal_skips_resolver_and_clear() {
        let writes = std::cell::RefCell::new(Vec::new());
        prepare_and_flash::<(), String, ()>(
            &[],
            |_| panic!("resolver called"),
            || {
                writes.borrow_mut().push("fw");
                Ok(())
            },
            |_| {
                writes.borrow_mut().push("clear");
                Ok(())
            },
            |_, _| panic!("install called"),
        )
        .unwrap();
        assert_eq!(*writes.borrow(), ["fw"]);
    }

    #[test]
    fn snapshot_hashes_the_bytes_kept_for_install() {
        let dir = std::env::temp_dir().join(format!("xpost-agent-snapshot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let firmware = dir.join("fw");
        let partitions = dir.join("partitions.csv");
        std::fs::write(&firmware, b"first firmware").unwrap();
        std::fs::write(
            &partitions,
            "factory,app,factory,0x10000,0x300000,\nxdp,0x40,0,0x310000,0x10000,\n",
        )
        .unwrap();
        let snapshot = firmware_snapshot(&firmware, &partitions).unwrap();
        std::fs::write(&firmware, b"changed firmware").unwrap();
        assert_eq!(snapshot.elf, b"first firmware");
        assert_eq!(
            snapshot.sha256,
            <[u8; 32]>::from(Sha256::digest(&snapshot.elf))
        );
        assert!(!snapshot.partition_table.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn standalone_snapshot_requires_existing_firmware_and_partitions() {
        let dir = std::env::temp_dir().join(format!("xpost-agent-fw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let firmware = dir.join("fw");
        let partitions = dir.join("partitions.csv");
        assert!(firmware_snapshot(&firmware, &partitions).is_err());
        std::fs::write(&firmware, b"abc").unwrap();
        assert!(firmware_snapshot(&firmware, &partitions).is_err());
        std::fs::write(&partitions, "invalid partition table").unwrap();
        assert!(firmware_snapshot(&firmware, &partitions).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_prepare_line_keeps_the_decoded_bytes() {
        let profile = "ab".repeat(32);
        let line = format!("perf prepare 00112233445566778899aabbccddeeff trace {profile}");
        match parse_app_command(&line).unwrap() {
            ParsedCmd::PerfPrepare {
                uuid,
                kind,
                profile,
            } => {
                assert_eq!(uuid[0], 0x00);
                assert_eq!(uuid[15], 0xff);
                assert_eq!(kind, PerfVariant::Trace);
                assert_eq!(profile[0], 0xab);
                assert_eq!(profile[31], 0xab);
            }
            _ => panic!("expected prepare"),
        }
        assert!(parse_app_command("perf prepare zz trace ab").is_err());
        assert!(parse_app_command("perf prepare 00 histogram ab").is_err());
    }

    #[test]
    fn a_prepare_reply_keeps_a_boot_id_past_u32() {
        let boot_id = u64::from(u32::MAX) + 9;
        let reply = OkReply::PerfPrepare { epoch: 3, boot_id };
        let msg = format_perf_session(&reply).unwrap();
        assert_eq!(msg, format!("OK perf prepare epoch=3 boot_id={boot_id}"));
    }
}
