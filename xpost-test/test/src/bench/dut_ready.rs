//! Wait for DUT UDP ctrl after agent flash (shared by `test all`).

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use rand::RngExt;
use test_common::hot_load_op::{HotLoadOp, KIND_READBACK, STORAGE_SRAM_A};

use crate::harness::Harness;

const DUT_READY_BUDGET: Duration = Duration::from_secs(180);
const PROBE: Duration = Duration::from_millis(500);
const CTRL_OP_WAIT: Duration = Duration::from_secs(5);

/// Consecutive ctrl QUERY successes required before bench (drop profiles boot slowly).
const DROP_READY_HITS: usize = 3;

fn post_flash_settle(profile_id: &str) -> Duration {
    match profile_id {
        // AOT + map + WiFi + TIMG1 idle_wdt — ctrl UDP binds after DHCP.
        "drop-xdp" => Duration::from_secs(test_common::drop_stats::POST_FLASH_SETTLE_DROP_XDP_SECS),
        "drop-hal" => Duration::from_secs(12),
        _ => Duration::from_secs(8),
    }
}

pub fn gen_lan_ip(esp: Ipv4Addr, avoid: &[Ipv4Addr]) -> Ipv4Addr {
    let mut rng = rand::rng();
    let oct = esp.octets();
    loop {
        let host = rng.random_range(2..254u8);
        let ip = Ipv4Addr::new(oct[0], oct[1], oct[2], host);
        if ip != esp && !avoid.contains(&ip) {
            return ip;
        }
    }
}

pub async fn wait_after_flash(harness: &Harness, profile_id: &str) -> Result<()> {
    let settle = post_flash_settle(profile_id);
    eprintln!("post-flash settle {}s ({profile_id}) …", settle.as_secs());
    tokio::time::sleep(settle).await;
    let ctrl = gen_lan_ip(harness.esp_ip, &[harness.esp_ip]);
    eprintln!(
        "waiting DUT ctrl after {profile_id} (esp={} ctrl_src={ctrl}) …",
        harness.esp_ip
    );
    let started = Instant::now();
    let need_hits = match profile_id {
        "drop-hal" | "drop-xdp" => DROP_READY_HITS,
        _ => 1,
    };
    let mut hits = 0usize;
    while started.elapsed() < DUT_READY_BUDGET {
        let ready = match profile_id {
            "drop-hal" | "drop-xdp" => harness.drop_stats_query(ctrl).await.is_ok(),
            "firmware" => firmware_ctrl_ready(harness, ctrl).await,
            _ => true,
        };
        if ready {
            hits += 1;
            if hits >= need_hits {
                eprintln!("DUT ctrl ready ({profile_id}, {hits}/{need_hits} QUERY ok)");
                return Ok(());
            }
        } else {
            hits = 0;
        }
        tokio::time::sleep(PROBE).await;
    }
    bail!(
        "DUT ctrl not ready within {} after flash `{profile_id}` \
         (WiFi in examples/*/config.toml, esp_ip={}, probe.reply_port match firmware)",
        fmt_secs(DUT_READY_BUDGET),
        harness.esp_ip
    );
}

async fn firmware_ctrl_ready(harness: &Harness, ctrl: Ipv4Addr) -> bool {
    harness
        .hot_load_op_wait(
            ctrl,
            HotLoadOp {
                kind: KIND_READBACK,
                region: 0,
                dram_slot: 0,
                src: STORAGE_SRAM_A,
                dst: 0,
                payload: &[],
            },
            CTRL_OP_WAIT,
        )
        .await
        .is_ok()
}

fn fmt_secs(d: Duration) -> String {
    format!("{}s", d.as_secs())
}
