//! Postgres ingest for drop benches (`xdp_drop_*`).

use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use log::warn;
use test_common::drop_stats::DropStats;
use tokio_postgres::Client;

use crate::bench::pg_dsn::{self, PgOpts, TableSet};
use crate::bench::stop::Stop;

pub struct Pg {
    client: Client,
    opts: PgOpts,
}

impl Pg {
    pub async fn connect(opts: PgOpts) -> Result<Self> {
        let client = pg_dsn::connect(opts).await?;
        pg_dsn::prepare(&client, opts, TableSet::Drop).await?;
        Ok(Self { client, opts })
    }

    async fn reconnect(&mut self) -> Result<()> {
        *self = Self::connect(self.opts).await?;
        Ok(())
    }

    pub async fn connect_retry(opts: PgOpts, stop: &Stop) -> Option<Self> {
        let mut n = 0u32;
        loop {
            if stop.is_set() {
                return None;
            }
            n += 1;
            match Self::connect(opts).await {
                Ok(pg) => return Some(pg),
                Err(e) => {
                    warn!("postgres connect #{n}: {e:#}");
                    stop.sleep_or(Duration::from_secs(5)).await;
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn ingest_until_ok(
        &mut self,
        path: &str,
        window_s: i32,
        flood: bool,
        hping_args: &str,
        eth_type: &str,
        ip_proto: i32,
        udp_dport: Option<i32>,
        elapsed_s: f64,
        tx_start_ts: SystemTime,
        tx_end_ts: SystemTime,
        tx_count: i64,
        host_pps: f64,
        dut_pps: Option<f64>,
        pkts_drop: i64,
        map_dump_ok: bool,
        stats: &DropStats,
        buckets: &[(i32, i64)],
        stop: &Stop,
    ) -> Option<i64> {
        let mut n = 0u32;
        loop {
            match self
                .ingest_once(
                    path,
                    window_s,
                    flood,
                    hping_args,
                    eth_type,
                    ip_proto,
                    udp_dport,
                    elapsed_s,
                    tx_start_ts,
                    tx_end_ts,
                    tx_count,
                    host_pps,
                    dut_pps,
                    pkts_drop,
                    map_dump_ok,
                    stats,
                    buckets,
                )
                .await
            {
                Ok(id) => return Some(id),
                Err(e) => {
                    if stop.is_set() {
                        warn!("postgres ingest aborted: {e:#}");
                        return None;
                    }
                    n += 1;
                    warn!("postgres ingest #{n}: {e:#}");
                    stop.sleep_or(Duration::from_secs(5)).await;
                    if let Err(re) = self.reconnect().await {
                        warn!("postgres reconnect: {re:#}");
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn ingest_once(
        &mut self,
        path: &str,
        window_s: i32,
        flood: bool,
        hping_args: &str,
        eth_type: &str,
        ip_proto: i32,
        udp_dport: Option<i32>,
        elapsed_s: f64,
        tx_start_ts: SystemTime,
        tx_end_ts: SystemTime,
        tx_count: i64,
        host_pps: f64,
        dut_pps: Option<f64>,
        pkts_drop: i64,
        map_dump_ok: bool,
        stats: &DropStats,
        buckets: &[(i32, i64)],
    ) -> Result<i64> {
        let tx = self.client.transaction().await.context("begin")?;
        let cfg = tx
            .query_one(
                "INSERT INTO xdp_drop_config \
                 (path, window_s, interferer, flood, hping_pps, hping_args, eth_type, ip_proto, udp_dport) \
                 VALUES ($1, $2, 'raw_udp', $3, NULL, $4, $5, $6, $7) RETURNING id",
                &[
                    &path,
                    &window_s,
                    &flood,
                    &hping_args,
                    &eth_type,
                    &ip_proto,
                    &udp_dport,
                ],
            )
            .await
            .context("INSERT xdp_drop_config")?;
        let config_id = pg_dsn::config_id_from_row(&cfg)?;
        let round = tx
            .query_one(
                "INSERT INTO xdp_drop_round \
                 (config_id, elapsed_s, tx_start_ts, tx_end_ts, tx_count, host_pps, dut_pps, \
                  pkts_drop, map_dump_ok, t_first_us, t_last_us) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING id",
                &[
                    &config_id,
                    &elapsed_s,
                    &tx_start_ts,
                    &tx_end_ts,
                    &tx_count,
                    &host_pps,
                    &dut_pps,
                    &pkts_drop,
                    &map_dump_ok,
                    &(stats.t_first_us as i64),
                    &(stats.t_last_us as i64),
                ],
            )
            .await
            .context("INSERT xdp_drop_round")?;
        let round_id = pg_dsn::config_id_from_row(&round)?;
        for &(ip_proto, pkt_count) in buckets {
            tx.execute(
                "INSERT INTO xdp_drop_rxcnt (config_id, round_id, ip_proto, pkt_count) \
                 VALUES ($1, $2, $3, $4)",
                &[&config_id, &round_id, &ip_proto, &pkt_count],
            )
            .await
            .context("INSERT xdp_drop_rxcnt")?;
        }
        tx.commit().await.context("commit")?;
        Ok(config_id)
    }
}
