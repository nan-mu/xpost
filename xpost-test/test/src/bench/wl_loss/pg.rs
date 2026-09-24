//! Postgres ingest for wl-loss (`xdp_loss_*`).

use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use log::warn;
use tokio_postgres::Client;

use crate::bench::pg_dsn::{self, PgOpts, TableSet};
use crate::bench::stop::Stop;

const INSERT_CONFIG: &str = "INSERT INTO xdp_loss_config \
    (interval_us, stream_len, udp_payload_len, hping_pps, window_s, interferer, stream_seed) \
    VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id";

#[derive(Clone, Debug)]
pub struct LossRow {
    pub t0: SystemTime,
    pub sent_pkts: i32,
    pub sent_bytes: i32,
    pub recv_bytes: i32,
    pub match_bytes: i32,
    pub loss_pct: Option<f64>,
    pub dump_ok: bool,
    pub dump_attempts: i32,
    pub sent_sha256: Option<String>,
    pub recv_sha256: Option<String>,
}

pub struct Pg {
    client: Client,
    opts: PgOpts,
}

impl Pg {
    pub async fn connect(opts: PgOpts) -> Result<Self> {
        let client = pg_dsn::connect(opts).await?;
        pg_dsn::prepare(&client, opts, TableSet::WlLoss).await?;
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

    pub async fn ingest_until_ok(
        &mut self,
        interval_us: i32,
        stream_len: i32,
        udp_payload_len: i32,
        hping_pps: i32,
        window_s: i32,
        interferer: &str,
        stream_seed: i64,
        row: &LossRow,
        stop: &Stop,
    ) -> Option<i64> {
        let mut n = 0u32;
        loop {
            match self
                .ingest_once(
                    interval_us,
                    stream_len,
                    udp_payload_len,
                    hping_pps,
                    window_s,
                    interferer,
                    stream_seed,
                    row,
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

    async fn ingest_once(
        &mut self,
        interval_us: i32,
        stream_len: i32,
        udp_payload_len: i32,
        hping_pps: i32,
        window_s: i32,
        interferer: &str,
        stream_seed: i64,
        row: &LossRow,
    ) -> Result<i64> {
        let tx = self.client.transaction().await.context("begin")?;
        let cfg = tx
            .query_one(
                INSERT_CONFIG,
                &[
                    &interval_us,
                    &stream_len,
                    &udp_payload_len,
                    &hping_pps,
                    &window_s,
                    &interferer,
                    &stream_seed,
                ],
            )
            .await
            .context("INSERT xdp_loss_config")?;
        let config_id = pg_dsn::config_id_from_row(&cfg)?;
        tx.execute(
            "INSERT INTO xdp_loss_data \
             (config_id, t0, sent_pkts, sent_bytes, recv_bytes, match_bytes, loss_pct, \
              dump_ok, dump_attempts, sent_sha256, recv_sha256) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
            &[
                &config_id,
                &row.t0,
                &row.sent_pkts,
                &row.sent_bytes,
                &row.recv_bytes,
                &row.match_bytes,
                &row.loss_pct,
                &row.dump_ok,
                &row.dump_attempts,
                &row.sent_sha256,
                &row.recv_sha256,
            ],
        )
        .await
        .context("INSERT xdp_loss_data")?;
        tx.commit().await.context("commit")?;
        Ok(config_id)
    }
}
