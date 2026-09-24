use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use log::warn;
use tokio_postgres::Client;

use crate::bench::pg_dsn::{self, PgOpts, TableSet};
use crate::bench::rate_common::{CELL_REPEATS, INTERFERER, P_PEAK, THRESH, Task};
use crate::bench::stop::Stop;
use crate::bench::wl_common::LEGIT_INTERVAL_US;

const INSERT_CONFIG: &str = "INSERT INTO xdp_rate_loss_config \
    (attack_pps,peak_pct,p_peak,thresh,mitigation_on,interval_us,stream_len,udp_payload_len,window_s,interferer,stream_seed,cell_repeat) \
    VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) RETURNING id";

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
    async fn connect(opts: PgOpts) -> Result<Self> {
        let client = pg_dsn::connect(opts).await?;
        pg_dsn::prepare(&client, opts, TableSet::RateLoss).await?;
        Ok(Self { client, opts })
    }

    pub async fn connect_retry(opts: PgOpts, stop: &Stop) -> Option<Self> {
        loop {
            if stop.is_set() {
                return None;
            }
            match Self::connect(opts).await {
                Ok(pg) => return Some(pg),
                Err(e) => {
                    warn!("postgres connect: {e:#}");
                    stop.sleep_or(Duration::from_secs(5)).await;
                }
            }
        }
    }

    async fn reconnect(&mut self) -> Result<()> {
        *self = Self::connect(self.opts).await?;
        Ok(())
    }

    pub async fn ingest_until_ok(
        &mut self,
        attack_pps: i32,
        task: Task,
        window_s: i32,
        stream_seed: i64,
        row: &LossRow,
        stop: &Stop,
    ) -> Option<i64> {
        loop {
            match self
                .ingest_once(attack_pps, task, window_s, stream_seed, row)
                .await
            {
                Ok(id) => return Some(id),
                Err(e) if stop.is_set() => {
                    warn!("postgres ingest aborted: {e:#}");
                    return None;
                }
                Err(e) => {
                    warn!("postgres ingest: {e:#}");
                    stop.sleep_or(Duration::from_secs(5)).await;
                    let _ = self.reconnect().await;
                }
            }
        }
    }

    async fn ingest_once(
        &mut self,
        attack_pps: i32,
        task: Task,
        window_s: i32,
        stream_seed: i64,
        row: &LossRow,
    ) -> Result<i64> {
        let tx = self.client.transaction().await.context("begin")?;
        let cfg = tx
            .query_one(
                INSERT_CONFIG,
                &[
                    &attack_pps,
                    &(task.peak_pct as i32),
                    &(P_PEAK as i32),
                    &(THRESH as i32),
                    &task.mitigation_on,
                    &(LEGIT_INTERVAL_US as i32),
                    &(super::STREAM_LEN as i32),
                    &(super::UDP_PAYLOAD_LEN as i32),
                    &window_s,
                    &INTERFERER,
                    &stream_seed,
                    &(CELL_REPEATS as i32),
                ],
            )
            .await
            .context("INSERT xdp_rate_loss_config")?;
        let config_id = pg_dsn::config_id_from_row(&cfg)?;
        tx.execute(
            "INSERT INTO xdp_rate_loss_data \
             (config_id,t0,sent_pkts,sent_bytes,recv_bytes,match_bytes,loss_pct,dump_ok,dump_attempts,sent_sha256,recv_sha256) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
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
        .context("INSERT xdp_rate_loss_data")?;
        tx.commit().await.context("commit")?;
        Ok(config_id)
    }
}
