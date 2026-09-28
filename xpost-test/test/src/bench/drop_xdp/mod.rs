//! `test drop-xdp` → `xdp_drop_*` (drop-xdp profile, boot AOT, stats API).

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use log::info;
use rand::RngExt;

use crate::harness::Harness;

use super::drop::pg;
use super::drop_common::{self, DropOpts};
use super::pg_dsn::PgOpts;
use super::stop::{self, Stop};
use super::udp_blast::{self, BLAST_THREADS};
use super::wl_common::fmt_dur;
use test_common::drop_stats;

pub const PATH_XDP_DROP: &str = "xdp_drop";

const READY_POLL: Duration = Duration::from_secs(180);

pub async fn run_drop_xdp(harness: &mut Harness, opts: DropOpts) -> Result<()> {
    let stop = Stop::new();
    stop::spawn_watch(stop.clone());

    let mut pg = if opts.no_pg {
        None
    } else {
        pg::Pg::connect_retry(
            PgOpts {
                formal: opts.formal,
            },
            &stop,
        )
        .await
    };
    if !opts.no_pg && pg.is_none() {
        eprintln!("stopped before postgres");
        return Ok(());
    };

    let window = opts.window();
    let rounds = opts.rounds();
    let ctrl = gen_lan_ip(harness.esp_ip, &[harness.esp_ip]);
    let spoof = gen_lan_ip(harness.esp_ip, &[harness.esp_ip, ctrl]);

    eprintln!(
        "drop-xdp {rounds} rounds  window={}s  blast=UDP×{BLAST_THREADS} 64B  iface={}  → xdp_drop_*  formal={}",
        window.as_secs(),
        harness.iface,
        opts.formal,
    );
    info!("drop-xdp ctrl={ctrl} spoof={spoof} esp={}", harness.esp_ip);

    if !wait_ctrl_ready(harness, ctrl, &stop).await {
        bail!("drop-xdp DUT ctrl not ready (drop stats QUERY)");
    }

    let pre_bench = Duration::from_secs(drop_stats::PRE_BENCH_SETTLE_DROP_XDP_SECS);
    eprintln!(
        "  pre-bench settle {}s (XDP/maps/ctrl) …",
        pre_bench.as_secs()
    );
    stop.sleep_or(pre_bench).await;

    let started = Instant::now();
    let mut done = 0usize;

    for r in 0..rounds {
        if stop.is_set() {
            break;
        }
        eprintln!("No. {}/{rounds}  UDP flood …", r + 1);
        if let Some((host_pps, dut_pps, config_id)) =
            exec_round(harness, ctrl, spoof, window, &stop, pg.as_mut(), opts.no_pg).await?
        {
            eprintln!(
                "  → host_pps={host_pps:.0}  dut_pps={}  pg={config_id:?}",
                dut_pps
                    .map(|p| format!("{p:.0}"))
                    .unwrap_or_else(|| "n/a".into())
            );
            done += 1;
        }
    }
    eprintln!(
        "drop-xdp done  {done}/{rounds}  elapsed {}",
        fmt_dur(started.elapsed())
    );
    Ok(())
}

const BENCH_DONE_TIMEOUT: Duration = Duration::from_secs(180);

async fn exec_round(
    harness: &mut Harness,
    ctrl: Ipv4Addr,
    spoof: Ipv4Addr,
    window: Duration,
    stop: &Stop,
    pg: Option<&mut pg::Pg>,
    no_pg: bool,
) -> Result<Option<(f64, Option<f64>, Option<i64>)>> {
    drop_common::reset_with_settle(harness, ctrl, stop)
        .await
        .context("drop stats RESET/settle")?;

    eprintln!("  blast until CAP or {}s max …", window.as_secs());
    let stop_blast = Arc::new(AtomicBool::new(false));
    let stop_blast2 = stop_blast.clone();
    let raw = harness.raw();
    let esp = harness.esp_ip;
    let echo_port = harness.echo_port;
    let max_window = window;

    let listen = async {
        let stats = harness.wait_bench_done(BENCH_DONE_TIMEOUT).await?;
        eprintln!(
            "  DUT bench_done notify  pkts_drop={}  t_us={}..{}",
            stats.drop_count, stats.t_first_us, stats.t_last_us
        );
        stop_blast2.store(true, Ordering::Release);
        Ok::<_, anyhow::Error>(stats)
    };

    let blast_task = tokio::task::spawn_blocking(move || {
        udp_blast::run_blast_until_stop(raw, spoof, esp, echo_port, stop_blast, max_window)
    });

    let (blast_res, stats_res) = tokio::join!(blast_task, listen);
    let blast = blast_res.context("blast task join")?;
    let stats = match stats_res {
        Ok(s) => s,
        Err(_) => {
            eprintln!("  no bench_done → fallback QUERY …");
            harness
                .drop_stats_query(ctrl)
                .await
                .context("drop stats QUERY fallback")?
        }
    };

    let pkts_drop = stats.drop_count;
    let host_pps = blast.host_pps();
    let dut_pps = stats.dut_pps();
    eprintln!(
        "  pkts_drop={pkts_drop}  tx_count={}  host_pps={host_pps:.0}  dut_pps={}  t_us={}..{}",
        blast.tx_count,
        dut_pps
            .map(|p| format!("{p:.0}"))
            .unwrap_or_else(|| "n/a".into()),
        stats.t_first_us,
        stats.t_last_us
    );
    drop_common::log_path_stats("drop-xdp", &stats, &blast);

    let tx_start = std::time::SystemTime::now() - blast.tx_end.duration_since(blast.tx_start);
    let tx_end = std::time::SystemTime::now();

    let config_id = if no_pg {
        None
    } else if let Some(pg) = pg {
        pg.ingest_until_ok(
            PATH_XDP_DROP,
            window.as_secs() as i32,
            true,
            "udp flood 64B",
            "ipv4",
            17,
            Some(harness.echo_port as i32),
            blast.elapsed_secs(),
            tx_start,
            tx_end,
            blast.tx_count as i64,
            host_pps,
            dut_pps,
            pkts_drop as i64,
            pkts_drop > 0,
            &stats,
            &[],
            stop,
        )
        .await
    } else {
        None
    };

    harness.reset_state().await;
    Ok(Some((host_pps, dut_pps, config_id)))
}

async fn wait_ctrl_ready(harness: &Harness, ctrl: Ipv4Addr, stop: &Stop) -> bool {
    const PROBE: Duration = Duration::from_millis(500);
    const NEED_HITS: usize = 3;
    let start = Instant::now();
    let mut hits = 0usize;
    while start.elapsed() < READY_POLL {
        if stop.is_set() {
            return false;
        }
        if harness.drop_stats_query(ctrl).await.is_ok() {
            hits += 1;
            if hits >= NEED_HITS {
                return true;
            }
        } else {
            hits = 0;
        }
        stop.sleep_or(PROBE).await;
    }
    false
}

fn gen_lan_ip(esp: Ipv4Addr, avoid: &[Ipv4Addr]) -> Ipv4Addr {
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
