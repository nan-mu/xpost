//! Function-call compute via `exec` ingress (Wi-Fi-free; default cagt).
//!
//! ```text
//! exec_bench (ebpf):
//!   agent flash fw exec-bench-ebpf
//!   for workload in {incr, square, bitswap, fib, buf, memcpy, fletcher32, bsort}:
//!     agent flash xdp workload
//!     agent clear perf
//!     agent update map workload
//!     agent enable exec
//!     agent get finish
//!     agent disable exec
//!     agent get perf
//!     agent get map
//!     upload to db
//!
//! exec_bench (hal):
//!   agent flash fw exec-bench-hal
//!   for workload in {…}:
//!     agent clear perf
//!     agent update map workload
//!     agent enable exec
//!     agent get finish
//!     agent disable exec
//!     agent get perf
//!     agent get map
//!     upload to db
//! ```

use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use exec_bench_common::{
    MAP_VALUE_LEN, OPCODES, expected_map32, parse_opcode, prepare_map32, verify_map32,
};

use crate::agent::AgentClient;
use crate::config::FileConfig;
use crate::db::{self, Firmware, PgOpts};
use crate::pipeline::{status, BudgetEta, READY_BUDGET, RoundUi};

const FLASH_IDLE: Duration = Duration::from_secs(300);
const MAP_NAME: &str = "workload";
const KEY_IN: &str = "0";
/// Dump encoding for 1-byte key `1` (see `format_hash_key`).
const KEY_OUT_DUMP: &str = "01";
const FW_EBPF: &str = "exec-bench-ebpf";
const FW_HAL: &str = "exec-bench-hal";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arm {
    Ebpf,
    Hal,
}

impl Arm {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ebpf" | "xdp" => Ok(Self::Ebpf),
            "hal" => Ok(Self::Hal),
            other => bail!("exec_bench arm must be ebpf|hal, got {other}"),
        }
    }

    pub fn as_db(self) -> &'static str {
        match self {
            Self::Ebpf => "ebpf",
            Self::Hal => "hal",
        }
    }

    pub fn fw_profile(self) -> &'static str {
        match self {
            Self::Ebpf => FW_EBPF,
            Self::Hal => FW_HAL,
        }
    }

    pub fn as_firmware(self) -> Firmware {
        match self {
            Self::Ebpf => Firmware::XdpCag,
            Self::Hal => Firmware::Hal,
        }
    }
}

pub struct ExecBenchOpts {
    pub formal: bool,
}

fn arms_from_cfg(arms: &[String]) -> Result<Vec<Arm>> {
    let mut out = Vec::new();
    for a in arms {
        out.push(Arm::parse(a)?);
    }
    if out.is_empty() {
        bail!("exec_bench.toml `arms` must list at least one of ebpf|hal");
    }
    out.dedup();
    Ok(out)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

fn hex_decode(s: &str) -> Result<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        bail!("odd hex length {}", s.len());
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = hex_nibble(bytes[i]).with_context(|| format!("hex digit {:?}", bytes[i] as char))?;
        let lo =
            hex_nibble(bytes[i + 1]).with_context(|| format!("hex digit {:?}", bytes[i + 1] as char))?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Host 32-byte workload blob for `op` (Compact-ELF params).
pub fn build_input(op: u32) -> Result<[u8; MAP_VALUE_LEN]> {
    let mut buf = [0u8; MAP_VALUE_LEN];
    prepare_map32(&mut buf, op).map_err(|e| anyhow::anyhow!("prepare_map32: {e:?}"))?;
    Ok(buf)
}

pub async fn run_exec_bench(cfg: &FileConfig, opts: ExecBenchOpts) -> Result<()> {
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .context("exec_bench needs [agent].endpoint in config/default.toml")?;
    let agent = AgentClient::new(endpoint.clone());
    let formal = opts.formal;
    let arms = arms_from_cfg(&cfg.exec_bench.arms)?;
    let workloads: Vec<&str> = OPCODES.iter().map(|(n, _)| *n).collect();

    RoundUi::banner(&format!(
        "exec_bench arms={:?} workloads={} formal={formal} agent={endpoint}",
        arms.iter().map(|a| a.as_db()).collect::<Vec<_>>(),
        workloads.len(),
    ));

    let pg_opts = PgOpts { formal };
    let mut pg = Some(db::connect_and_ensure(pg_opts).await?);

    let mut eta = BudgetEta::new();
    eta.add_flashes(arms.len());
    // Rough: one flash_xdp per eBPF workload + settle budget per cell.
    let ebpf_cells = if arms.contains(&Arm::Ebpf) {
        workloads.len()
    } else {
        0
    };
    eta.add_flashes(ebpf_cells);
    eta.add_fixed(READY_BUDGET.saturating_mul((arms.len() * workloads.len()) as u32));

    let ui = RoundUi::new();
    let exam_id = if let Some(client) = pg.as_mut() {
        ui.check_start("INSERT exam");
        let id = db::insert_exam(client, Some("exec_bench")).await?;
        ui.check_ok(&format!("exam={id}"));
        Some(id)
    } else {
        None
    };

    for arm in &arms {
        flash_fw(cfg, &agent, &ui, &mut eta, arm.fw_profile()).await?;

        for &wl in &workloads {
            let op = parse_opcode(wl).context("opcode")?;
            if *arm == Arm::Ebpf {
                ui.check_start(&format!("flash xdp `{wl}`"));
                agent.flash_xdp(wl).await.with_context(|| format!("flash xdp {wl}"))?;
                ui.check_ok(wl);
            }

            let input = build_input(op)?;
            let input_hex = hex_encode(&input);

            agent.clear_perf().await.context("clear/perf")?;
            agent
                .update_map_batch(MAP_NAME, &[(KEY_IN.to_string(), input_hex.clone())])
                .await
                .context("update/map workload")?;

            let started_at = SystemTime::now();
            let wall0 = Instant::now();
            let enable = agent.enable_exec().await;
            let finish = match &enable {
                Ok(_) => agent.get_finish().await,
                Err(e) => Err(anyhow::anyhow!("enable exec: {e:#}")),
            };
            let _ = agent.disable_exec().await;
            let stopped_at = SystemTime::now();
            let wall = wall0.elapsed();

            let mut output: Option<Vec<u8>> = None;
            let mut ok = false;
            match (&enable, &finish) {
                (Ok(_), Ok(msg)) => {
                    if !msg.contains("OK finish") && msg != "OK finish" {
                        // Agent may return the transcript line or a short OK.
                        log::info!("get_finish message: {msg}");
                    }
                    match read_output(&agent).await {
                        Ok(bytes) => {
                            ok = verify_map32(&bytes)
                                || expected_map32(op)
                                    .map(|exp| bytes.as_slice() == exp.as_slice())
                                    .unwrap_or(false);
                            output = Some(bytes);
                        }
                        Err(e) => {
                            eprintln!("    WARN: {arm:?}/{wl} get map: {e:#}");
                        }
                    }
                }
                _ => {
                    eprintln!(
                        "    WARN: {:?}/{wl} enable/finish failed: enable={enable:?} finish={finish:?}",
                        arm.as_db()
                    );
                }
            }

            ui.permanent(&format!(
                "    {}/{wl}: ok={ok} wall_ms={} out_len={}",
                arm.as_db(),
                wall.as_millis(),
                output.as_ref().map(|b| b.len()).unwrap_or(0),
            ));

            if !ok {
                bail!(
                    "exec_bench {}/{} failed (enable={:?} finish={:?})",
                    arm.as_db(),
                    wl,
                    enable.as_ref().map(|_| "ok").map_err(|e| format!("{e:#}")),
                    finish.as_ref().map(|m| m.as_str()).map_err(|e| format!("{e:#}")),
                );
            }

            if let (Some(client), Some(exam_id)) = (pg.as_mut(), exam_id) {
                ui.check_start(&format!("写入 {}/{wl}", arm.as_db()));
                let cfg_id = db::insert_exec_bench_config(
                    client,
                    exam_id,
                    arm.as_db(),
                    wl,
                    &input,
                    started_at,
                    stopped_at,
                )
                .await?;
                db::insert_exec_bench_data(client, cfg_id, 1, ok, output.as_deref()).await?;
                let msg = agent.get_perf().await.context("get/perf")?;
                if msg.is_empty() {
                    bail!("{}/{wl}: get/perf empty after ok cell", arm.as_db());
                }
                let decoded = db::decode_report_msg(&msg)?;
                db::write_perf(
                    client,
                    exam_id,
                    cfg_id,
                    arm.as_firmware(),
                    db::PerfExperiment::ExecBench,
                    &decoded,
                )
                .await?;
                ui.check_ok(&format!("config={cfg_id} mask={:#x}", decoded.mask));
            }

            eta.consume_fixed(Duration::from_secs(5));
            ui.set_eta(&eta.fmt());
        }
    }

    if let (Some(client), Some(exam_id)) = (pg.as_mut(), exam_id) {
        db::stop_exam(client, exam_id).await?;
    }
    RoundUi::banner(&format!(
        "exec_bench done arms={} workloads={}",
        arms.len(),
        workloads.len()
    ));
    Ok(())
}

async fn read_output(agent: &AgentClient) -> Result<Vec<u8>> {
    let map = agent.get_map(MAP_NAME).await?;
    for e in &map.entries {
        if e.key == KEY_OUT_DUMP || e.key == "1" {
            if e.value.chars().all(|c| c.is_ascii_hexdigit()) && e.value.len() == MAP_VALUE_LEN * 2
            {
                return hex_decode(&e.value);
            }
            // Decimal u64 dump (legacy ≤8): not enough for 32B verify.
            bail!("output value not hex-32: {:?}", e.value);
        }
    }
    bail!("get/map {MAP_NAME} missing output key {KEY_OUT_DUMP}");
}

async fn flash_fw(
    cfg: &FileConfig,
    agent: &AgentClient,
    ui: &RoundUi,
    eta: &mut BudgetEta,
    profile: &str,
) -> Result<()> {
    ui.begin_round();
    ui.set_eta(&eta.fmt());
    ui.check_pending(&format!("烧录 `{profile}`"));
    ui.check_pending(&format!("settle ≤{}s", READY_BUDGET.as_secs()));
    ui.check_pending("DUT ready");

    if let Err(e) = status::flash_idle(ui, agent, profile, FLASH_IDLE, eta).await {
        ui.clear_temp_for_error();
        return Err(e);
    }
    eta.consume_fixed(READY_BUDGET);
    if let Err(e) = status::settle_checklist(ui, agent, READY_BUDGET).await {
        ui.clear_temp_for_error();
        return Err(e);
    }
    ui.set_eta(&eta.fmt());
    ui.check_activate("DUT ready");
    ui.check_ok(profile);
    ui.end_round();
    let _ = cfg;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use exec_bench_common::{OP_BSORT, OP_INCR, run_hal};

    #[test]
    fn map32_incr_verifies() {
        let mut buf = build_input(OP_INCR).unwrap();
        run_hal(&mut buf).unwrap();
        assert!(verify_map32(&buf));
        assert_eq!(buf[8..12], 1u32.to_le_bytes());
    }

    #[test]
    fn map32_bsort_verifies() {
        let mut buf = build_input(OP_BSORT).unwrap();
        run_hal(&mut buf).unwrap();
        assert!(verify_map32(&buf));
    }

    #[test]
    fn hex_roundtrip_32() {
        let buf = build_input(OP_INCR).unwrap();
        let h = hex_encode(&buf);
        assert_eq!(h.len(), 64);
        let back = hex_decode(&h).unwrap();
        assert_eq!(back, buf);
    }
}
