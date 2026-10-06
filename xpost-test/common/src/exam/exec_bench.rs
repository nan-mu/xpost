//! 测量 DUT 上 **无 Wi-Fi 的 `exec` 入口** 做一次函数计算（相对 HAL 对照）。
//!
//! eBPF 臂加载 `libbpf-mcu/tests/xdp/<name>.c` 打出的 XDP ELF。HAL 臂在
//! `exec_bench_hal::run_hal` 上跑同一 32 字节布局。每格结束后读 `get perf`：
//! eBPF 报告含 Counter/Aggregate/Gauge/Trace（`xdp_begin`/`xdp_end` 各应至少
//! 一次）；HAL 报告同样是 CAGT，`xdp_begin` 应至少一次。缺少这些则本格 `bail`。
//!
//! ```text
//! exec_bench:
//!   for fw in {ebpf, hal}:
//!     agent flash fw
//!     for workload in {incr, square, bitswap, fib, buf, memcpy, fletcher32, bsort}:
//!       if ebpf:
//!         agent clear xdp *
//!         agent flash xdp workload
//!         agent get init
//!       agent clear perf
//!       agent update map(workload) key 0
//!       agent enable exec
//!       agent get finish
//!       agent disable exec
//!       agent get map(workload)
//!       agent get perf
//!       upload to db
//! ```
//!
//! 主机只走 agent map 协议（`update map` / `get map` 的 32 字节 hex），不解释
//! opcode。固件约定值布局为 `[0..4]` LE opcode、`[4..8]` LE param、`[8..]` payload。
//! eBPF 臂把 map[0] 拷进 `exec_call::invoke`，再把结果写回 map[1]；镜像用
//! `bpf_printk` 打出 `OK finish`。HAL 臂对同一 32 字节做 `run_hal`，再在 UART
//! 上打出同样的 `OK finish`。本模块用固定 hex fixture 比对 map[1]。eBPF 每个
//! workload 先 `clear xdp *` 再 `flash xdp`，使新容器落在 offset 0。
//!
//! **数据处理**
//!
//! 每格写入 `exec_bench_config` + `exec_bench_data`（DDL：`sql/exec_bench.sql`）。
//! eBPF 的 `get perf` 写入共享 `xdp_*` 表，并把 Trace 写入 `trace_event`
//!（collector 标签 `xdp_t`）。HAL 的 CAGT 写入 `exec_bench_hal_*`，Trace 写入
//! 同一 `trace_event`（`config_id` 指向 HAL 格）。
//! 格失败则 `bail`，该格不入库。
//!
//! `exec_bench_config`：
//! - `exam_id` — 本轮 `exam` 行
//! - `firmware` — 当前臂（`ebpf` / `hal`，实验列名，不是 SQL `firmware` 枚举）
//! - `workload` — XDP stem / opcode 名（`incr` … `bsort`）
//! - `input` — 写入 map[0] 的 32 字节
//! - `started_at` / `stopped_at` — 该格 `enable exec` 到 `disable exec` 的墙钟
//!
//! `exec_bench_data`（每格一行，`rep` 当前恒为 1）：
//! - `config_id` — 所属 `exec_bench_config`
//! - `rep` — 重复序号
//! - `ok` — `enable`/`finish` 成功且 map[1] 与期望 32 字节一致
//! - `output` — `agent get map(workload)` 读到的 map[1] 32 字节

use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};

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
const MAP_VALUE_LEN: usize = 32;

/// Wire fixtures for `update map` / `get map` (64 hex chars = 32 bytes).
/// Names match cold XDP image stems under the eBPF profile.
const WORKLOADS: &[Workload] = &[
    Workload {
        name: "incr",
        input_hex: "0000000000000000000000000000000000000000000000000000000000000000",
        expect_hex: "0000000000000000010000000000000000000000000000000000000000000000",
    },
    Workload {
        name: "square",
        input_hex: "0100000000000000030000000000000000000000000000000000000000000000",
        expect_hex: "0100000000000000090000000000000000000000000000000000000000000000",
    },
    Workload {
        name: "bitswap",
        input_hex: "02000000000000005a5aa5a50000000000000000000000000000000000000000",
        expect_hex: "0200000000000000a5a55a5a0000000000000000000000000000000000000000",
    },
    Workload {
        name: "fib",
        input_hex: "030000001e000000000000000000000000000000000000000000000000000000",
        expect_hex: "030000001e00000028b20c000000000000000000000000000000000000000000",
    },
    Workload {
        name: "buf",
        input_hex: "0400000010000000031425364758697a8b9cadbecfe0f1020000000000000000",
        expect_hex: "0400000010000000031527394b5d6f8193a5b7c9dbedff11a007000000000000",
    },
    Workload {
        name: "memcpy",
        input_hex: "0500000008000000031425364758697a00000000000000000000000000000000",
        expect_hex: "0500000008000000031425364758697a031425364758697a0000000000000000",
    },
    Workload {
        name: "fletcher32",
        input_hex: "0600000010000000031425364758697a8b9cadbecfe0f1020000000000000000",
        expect_hex: "0600000010000000031425364758697a8b9cadbecfe0f102d35ba10300000000",
    },
    Workload {
        name: "bsort",
        input_hex: "07000000060000009d601517e4e6dd782b6da6da72f36e3cb979379e00000000",
        expect_hex: "0700000006000000000000009d60151772f36e3ce4e6dd78b979379e2b6da6da",
    },
];

#[derive(Clone, Copy)]
struct Workload {
    name: &'static str,
    input_hex: &'static str,
    expect_hex: &'static str,
}

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
            Self::Ebpf => Firmware::XdpT,
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

fn map_value_bytes(hex: &str) -> Result<Vec<u8>> {
    let bytes = hex_decode(hex)?;
    if bytes.len() != MAP_VALUE_LEN {
        bail!("map value must be {MAP_VALUE_LEN} bytes, got {}", bytes.len());
    }
    Ok(bytes)
}

pub async fn run_exec_bench(cfg: &FileConfig, opts: ExecBenchOpts) -> Result<()> {
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .context("exec_bench needs [agent].endpoint in config/default.toml")?;
    let agent = AgentClient::new(endpoint.clone());
    let formal = opts.formal;
    let arms = arms_from_cfg(&cfg.exec_bench.arms)?;
    let workloads = WORKLOADS;

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

        for wl in workloads {
            if *arm == Arm::Ebpf {
                // Clear first so the new name is Created at offset 0 (sector-aligned).
                agent
                    .clear_xdp("*")
                    .await
                    .context("clear/xdp before flash_xdp")?;
                agent
                    .flash_xdp(wl.name)
                    .await
                    .with_context(|| format!("flash/xdp {}", wl.name))?;
                agent
                    .get_init()
                    .await
                    .with_context(|| format!("OK init after flash/xdp {}", wl.name))?;
            }

            let input = map_value_bytes(wl.input_hex)?;
            let expect = map_value_bytes(wl.expect_hex)?;

            agent.clear_perf().await.context("clear/perf")?;
            agent
                .update_map_batch(MAP_NAME, &[(KEY_IN.to_string(), wl.input_hex.to_string())])
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
                            ok = bytes.as_slice() == expect.as_slice();
                            output = Some(bytes);
                        }
                        Err(e) => {
                            eprintln!("    WARN: {arm:?}/{} get map: {e:#}", wl.name);
                        }
                    }
                }
                _ => {
                    eprintln!(
                        "    WARN: {:?}/{} enable/finish failed: enable={enable:?} finish={finish:?}",
                        arm.as_db(),
                        wl.name
                    );
                }
            }

            ui.permanent(&format!(
                "    {}/{}: ok={ok} wall_ms={} out_len={}",
                arm.as_db(),
                wl.name,
                wall.as_millis(),
                output.as_ref().map(|b| b.len()).unwrap_or(0),
            ));

            if !ok {
                bail!(
                    "exec_bench {}/{} failed (enable={:?} finish={:?})",
                    arm.as_db(),
                    wl.name,
                    enable.as_ref().map(|_| "ok").map_err(|e| format!("{e:#}")),
                    finish.as_ref().map(|m| m.as_str()).map_err(|e| format!("{e:#}")),
                );
            }

            if let (Some(client), Some(exam_id)) = (pg.as_mut(), exam_id) {
                ui.check_start(&format!("写入 {}/{}", arm.as_db(), wl.name));
                let cfg_id = db::insert_exec_bench_config(
                    client,
                    exam_id,
                    arm.as_db(),
                    wl.name,
                    &input,
                    started_at,
                    stopped_at,
                )
                .await?;
                db::insert_exec_bench_data(client, cfg_id, 1, ok, output.as_deref()).await?;
                let msg = agent.get_perf().await.context("get/perf")?;
                if msg.is_empty() {
                    bail!("{}/{}: get/perf empty after ok cell", arm.as_db(), wl.name);
                }
                let decoded = db::decode_report_msg(&msg)?;
                match arm {
                    Arm::Ebpf | Arm::Hal => {
                        if decoded.mask & db::MASK_T == 0 {
                            bail!("{}/{}: get/perf missing Trace section (mask={:#x})", arm.as_db(), wl.name, decoded.mask);
                        }
                        let n_begin = decoded
                            .trace
                            .as_ref()
                            .map(|t| t.events.iter().filter(|e| e.kind == 2).count())
                            .unwrap_or(0);
                        if n_begin == 0 {
                            bail!("{}/{}: Trace has no xdp_begin", arm.as_db(), wl.name);
                        }
                    }
                }
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

    fn hex_encode(bytes: &[u8]) -> String {
        const HEX: &[u8] = b"0123456789abcdef";
        let mut out = String::with_capacity(bytes.len() * 2);
        for &b in bytes {
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0xf) as usize] as char);
        }
        out
    }

    #[test]
    fn fixtures_are_32_byte_hex() {
        for wl in WORKLOADS {
            let input = map_value_bytes(wl.input_hex).unwrap();
            let expect = map_value_bytes(wl.expect_hex).unwrap();
            assert_eq!(input.len(), MAP_VALUE_LEN, "{}", wl.name);
            assert_eq!(expect.len(), MAP_VALUE_LEN, "{}", wl.name);
            assert_ne!(
                wl.input_hex, wl.expect_hex,
                "{} input must differ from expect",
                wl.name
            );
        }
    }

    #[test]
    fn hex_roundtrip_incr() {
        let wl = &WORKLOADS[0];
        let buf = map_value_bytes(wl.input_hex).unwrap();
        let h = hex_encode(&buf);
        assert_eq!(h, wl.input_hex);
        assert_eq!(hex_decode(&h).unwrap(), buf);
    }
}
