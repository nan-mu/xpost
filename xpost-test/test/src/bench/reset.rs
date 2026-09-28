//! Shared DUT reboot helpers (HW GPIO listener optional, KIND_REBOOT fallback).

use std::net::Ipv4Addr;
use std::time::Duration;

use log::{debug, info, warn};
use test_common::hot_load_op::{HotLoadOp, KIND_REBOOT};
use tokio::io::AsyncWriteExt;

use crate::bench::stop::Stop;
use crate::harness::Harness;

pub const RESET_HOST: &str = "192.168.110.96";
pub const RESET_PORT: u16 = 9473;

/// Try Mac GPIO reset listener; fall back to firmware `KIND_REBOOT` over UDP ctrl.
pub async fn reboot_dut(harness: &Harness, ctrl: Ipv4Addr, label: &str, stop: &Stop) {
    if try_hw_reset(label).await {
        stop.sleep_or(Duration::from_millis(200)).await;
        return;
    }
    let op = HotLoadOp {
        kind: KIND_REBOOT,
        region: 0,
        dram_slot: 0,
        src: 0,
        dst: 0,
        payload: &[],
    };
    match harness.hot_load_op(ctrl, op).await {
        Ok(ack) => info!("{label} REBOOT ack status={}", ack.status),
        Err(e) => debug!("{label} REBOOT no ack ({e:#})"),
    }
    stop.sleep_or(Duration::from_millis(200)).await;
}

async fn try_hw_reset(label: &str) -> bool {
    let Ok(Ok(mut s)) = tokio::time::timeout(
        Duration::from_millis(400),
        tokio::net::TcpStream::connect((RESET_HOST, RESET_PORT)),
    )
    .await
    else {
        debug!("{label}: HW reset listener {RESET_HOST}:{RESET_PORT} unavailable");
        return false;
    };
    match s.write_all(b"reset\n").await {
        Ok(()) => {
            let _ = s.shutdown().await;
            info!("{label} HW reset via {RESET_HOST}:{RESET_PORT}");
            true
        }
        Err(e) => {
            warn!("{label} HW reset write failed ({e:#})");
            false
        }
    }
}
