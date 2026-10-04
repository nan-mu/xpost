//! HAL smoke pipeline (`run = ["dev"]`).
//!
//! Flashes each `*-hal` profile, exercises map / enable / short stim, and
//! asserts dataplane outcomes. No XDP arms, no Postgres, perf ignored.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::agent::AgentClient;
use crate::config::FileConfig;
use crate::exam::just_drop::stim_src_ip;
use crate::pipeline::{
    status, BudgetEta, Harness, READY_BUDGET, RawTx, RoundUi, iface_ipv4, iface_mac,
};

const STIM_PORT: u16 = 8080;
const IPPROTO_UDP: u32 = 17;
const FLASH_IDLE: Duration = Duration::from_secs(300);
const SMOKE_WINDOW: Duration = Duration::from_secs(2);
const SMOKE_PPS: f64 = 100.0;
const DRAIN: Duration = Duration::from_millis(400);
const SOURCE_SEED: i64 = 0x00C0_FFEE;

/// Run HAL-only smoke for just_tx / just_drop / firewall (no XDP).
pub async fn run_dev(cfg: &FileConfig, harness: &mut Harness) -> Result<()> {
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .context("dev needs [agent].endpoint in config/default.toml")?;
    let agent = AgentClient::new(endpoint.clone());
    let esp_s = cfg.target.esp_ip.to_string();

    RoundUi::banner(&format!(
        "dev HAL smoke (just_tx/just_drop/firewall) @{SMOKE_PPS:.0}pps {}s agent={endpoint} esp={esp_s}",
        SMOKE_WINDOW.as_secs()
    ));

    let mut eta = BudgetEta::new();
    eta.add_flashes(3);
    eta.add_fixed((READY_BUDGET + SMOKE_WINDOW + DRAIN).saturating_mul(3));

    smoke_just_tx(cfg, harness, &agent, &mut eta).await?;
    smoke_just_drop(cfg, &agent, &mut eta).await?;
    smoke_firewall(cfg, harness, &agent, &mut eta).await?;

    RoundUi::banner("dev HAL smoke done");
    Ok(())
}

async fn flash_settle(
    agent: &AgentClient,
    ui: &RoundUi,
    eta: &mut BudgetEta,
    profile: &str,
) -> Result<()> {
    ui.begin_round();
    ui.check_pending(&format!("烧录 `{profile}`"));
    ui.check_pending(&format!("settle ≤{}s", READY_BUDGET.as_secs()));
    ui.set_eta(&eta.fmt());
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
    ui.check_ok("hal");
    Ok(())
}

async fn smoke_just_tx(
    cfg: &FileConfig,
    harness: &mut Harness,
    agent: &AgentClient,
    eta: &mut BudgetEta,
) -> Result<()> {
    let ui = RoundUi::new();
    flash_settle(agent, &ui, eta, "just-tx-hal").await?;

    let spoof = iface_ipv4(&cfg.probe.iface)
        .unwrap_or_else(|_| lan_ip(cfg.target.esp_ip, &[cfg.target.esp_ip]));

    agent
        .clear_map("rxcnt")
        .await
        .context("just_tx_hal clear/map rxcnt")?;
    let before = agent.map_slot("rxcnt", IPPROTO_UDP).await?;

    harness.reset_state().await;
    let pps = SMOKE_PPS.max(1.0);
    let interval = Duration::from_secs_f64(1.0 / pps);
    let start = Instant::now();
    let deadline = start + SMOKE_WINDOW;
    let mut seqs = Vec::new();
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;
    while Instant::now() < deadline {
        ticker.tick().await;
        if Instant::now() >= deadline {
            break;
        }
        let seq = harness.alloc_seq();
        if harness.send_stimulus(spoof, None, seq).await.is_ok() {
            seqs.push(seq);
        }
    }
    tokio::time::sleep(DRAIN).await;

    let mut ok = 0u64;
    for seq in &seqs {
        let sight = harness.take_seq(*seq).await;
        if sight.t0_ns.is_some() && sight.t_bounce_ns.or(sight.t_echo_ns).is_some() {
            ok += 1;
        }
    }
    let after = agent.map_slot("rxcnt", IPPROTO_UDP).await?;
    let delta = after.saturating_sub(before);
    let sent = seqs.len() as u64;
    ui.permanent(&format!(
        "    just_tx_hal: sent={sent} ok={ok} rxcntΔ={delta}"
    ));
    if sent == 0 {
        ui.clear_temp_for_error();
        bail!("just_tx_hal: sent=0");
    }
    if ok == 0 {
        ui.clear_temp_for_error();
        bail!("just_tx_hal: no echo/bounce sightings (ok=0/{sent})");
    }
    if delta == 0 {
        ui.clear_temp_for_error();
        bail!("just_tx_hal: rxcnt slot {IPPROTO_UDP} unchanged");
    }
    ui.end_round();
    eta.consume_fixed(SMOKE_WINDOW + DRAIN);
    Ok(())
}

async fn smoke_just_drop(
    cfg: &FileConfig,
    agent: &AgentClient,
    eta: &mut BudgetEta,
) -> Result<()> {
    let ui = RoundUi::new();
    flash_settle(agent, &ui, eta, "just-drop-hal").await?;

    agent
        .clear_map("counter")
        .await
        .context("just_drop_hal clear/map counter")?;
    agent
        .enable_xdp()
        .await
        .context("just_drop_hal enable/xdp (arm DROP)")?;

    let sent = paced_multi_src(cfg, &ui, eta, SMOKE_WINDOW, SMOKE_PPS, SOURCE_SEED).await?;
    tokio::time::sleep(DRAIN).await;
    agent
        .disable_xdp()
        .await
        .context("just_drop_hal disable/xdp")?;

    let map = agent.get_map("counter").await.context("just_drop_hal get/map")?;
    let mut sum = 0i64;
    let mut slots = 0u32;
    for e in &map.entries {
        let Ok(slot) = e.key.parse::<usize>() else {
            continue;
        };
        let Ok(value) = e.value.parse::<i64>() else {
            continue;
        };
        if slot < 256 {
            sum += value;
            if value != 0 {
                slots += 1;
            }
        }
    }
    ui.permanent(&format!(
        "    just_drop_hal: sent={sent} mapΣ={sum} nonzero_slots={slots}"
    ));
    if sent == 0 {
        ui.clear_temp_for_error();
        bail!("just_drop_hal: sent=0");
    }
    if sum == 0 {
        ui.clear_temp_for_error();
        bail!("just_drop_hal: counter map sum=0 after stim");
    }
    ui.end_round();
    eta.consume_fixed(SMOKE_WINDOW + DRAIN);
    Ok(())
}

async fn smoke_firewall(
    cfg: &FileConfig,
    harness: &mut Harness,
    agent: &AgentClient,
    eta: &mut BudgetEta,
) -> Result<()> {
    let ui = RoundUi::new();
    flash_settle(agent, &ui, eta, "firewall-hal").await?;

    let esp = cfg.target.esp_ip;
    let host = iface_ipv4(&cfg.probe.iface).ok();
    let mut avoid = vec![esp];
    if let Some(h) = host {
        avoid.push(h);
    }
    let hit = lan_ip(esp, &avoid);
    let miss = lan_ip(esp, &[esp, hit]);

    agent
        .clear_map("blacklist")
        .await
        .context("firewall_hal clear/map")?;
    // Value must be non-zero: HAL `is_blocked` and XDP `*flag` both require it.
    agent
        .update_map_batch("blacklist", &[(hit.to_string(), "1".into())])
        .await
        .context("firewall_hal update/map seed")?;
    let rows = agent.get_map("blacklist").await.context("firewall_hal get/map")?;
    if !rows.entries.iter().any(|e| e.key == hit.to_string() && e.value == "1") {
        ui.clear_temp_for_error();
        bail!("firewall_hal: seeded hit={hit} missing from get/map");
    }

    agent
        .enable_xdp()
        .await
        .context("firewall_hal enable/xdp")?;

    let hit_ok = probe_echoes(harness, hit, 32).await?;
    let miss_ok = probe_echoes(harness, miss, 32).await?;

    agent
        .disable_xdp()
        .await
        .context("firewall_hal disable/xdp")?;
    let _ = agent.clear_map("blacklist").await;

    ui.permanent(&format!(
        "    firewall_hal: hit={hit} echo={hit_ok}/32 miss={miss} echo={miss_ok}/32"
    ));
    if hit_ok > 2 {
        ui.clear_temp_for_error();
        bail!("firewall_hal: expected hit src dropped, got {hit_ok} echoes");
    }
    if miss_ok < 8 {
        ui.clear_temp_for_error();
        bail!("firewall_hal: expected miss src echoed, got {miss_ok}/32");
    }
    ui.end_round();
    eta.consume_fixed(SMOKE_WINDOW + DRAIN);
    Ok(())
}

async fn probe_echoes(harness: &mut Harness, src: Ipv4Addr, n: u32) -> Result<u64> {
    harness.reset_state().await;
    let mut seqs = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let seq = harness.alloc_seq();
        if harness.send_stimulus(src, None, seq).await.is_ok() {
            seqs.push(seq);
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::sleep(DRAIN).await;
    let mut ok = 0u64;
    for seq in &seqs {
        let sight = harness.take_seq(*seq).await;
        if sight.t_bounce_ns.or(sight.t_echo_ns).is_some() {
            ok += 1;
        }
    }
    Ok(ok)
}

async fn paced_multi_src(
    cfg: &FileConfig,
    ui: &RoundUi,
    eta: &BudgetEta,
    window: Duration,
    target_pps: f64,
    seed: i64,
) -> Result<u64> {
    let esp = cfg.target.esp_ip;
    let esp_mac = cfg.esp_mac_bytes()?;
    let src_mac = match cfg.src_mac_override()? {
        Some(m) => m,
        None => iface_mac(&cfg.probe.iface)?,
    };
    let raw = RawTx::open(
        &cfg.probe.iface,
        src_mac,
        esp_mac,
        esp,
        STIM_PORT,
        cfg.target.stim_sport,
    )
    .context("RawTx::open")?;

    let pps = target_pps.max(1.0);
    let interval = Duration::from_secs_f64(1.0 / pps);
    let expect = (pps * window.as_secs_f64()).max(1.0);
    let start = Instant::now();
    let deadline = start + window;
    let mut sent = 0u64;
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;

    ui.live_set("hal drop", &[("tx", 0.0, "0")]);
    ui.set_eta(&eta.fmt());
    let mut last_paint = Instant::now() - Duration::from_secs(1);

    while Instant::now() < deadline {
        ticker.tick().await;
        if Instant::now() >= deadline {
            break;
        }
        let spoof = stim_src_ip(seed, sent, esp);
        if raw.send_udp(spoof, esp, STIM_PORT, b"D").is_ok() {
            sent += 1;
        }
        if last_paint.elapsed() >= Duration::from_millis(100) {
            let frac = (sent as f64 / expect).clamp(0.0, 1.0);
            ui.live_set(
                "hal drop",
                &[("tx", frac, &format!("{sent}/{expect:.0}"))],
            );
            ui.set_eta(&eta.fmt());
            last_paint = Instant::now();
        }
    }
    ui.live_clear();
    Ok(sent)
}

fn lan_ip(esp: Ipv4Addr, avoid: &[Ipv4Addr]) -> Ipv4Addr {
    let o = esp.octets();
    for host in 2u8..255 {
        let ip = Ipv4Addr::new(o[0], o[1], o[2], host);
        if ip != esp && !avoid.contains(&ip) {
            return ip;
        }
    }
    Ipv4Addr::new(o[0], o[1], o[2], 254)
}
