use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use log::warn;
use tokio_postgres::Client;

use crate::bench::pg_dsn::{self, PgOpts, TableSet};
use crate::bench::rate_common::{CELL_REPEATS, INTERFERER, P_PEAK, THRESH, Task};
use crate::bench::stop::Stop;

const INSERT_CONFIG: &str = "INSERT INTO xdp_rate_rtt_config \
    (attack_pps, peak_pct, p_peak, thresh, mitigation_on, window_s, probe_interval_ms, cell_repeat, interferer) \
    VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING id";

#[derive(Clone, Debug)]
pub struct ProbeRtt {
    pub t0: SystemTime,
    pub seq: i32,
    pub ok: bool,
    pub rtt_ms: Option<f64>,
}

pub struct Pg {
    client: Client,
    opts: PgOpts,
}

impl Pg {
    async fn connect(opts: PgOpts) -> Result<Self> {
        let client = pg_dsn::connect(opts).await?;
        pg_dsn::prepare(&client, opts, TableSet::RateRtt).await?;
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
        samples: &[ProbeRtt],
        stop: &Stop,
    ) -> Option<i64> {
        loop {
            match self.ingest_once(attack_pps, task, window_s, samples).await {
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
        samples: &[ProbeRtt],
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
                    &window_s,
                    &50i32,
                    &(CELL_REPEATS as i32),
                    &INTERFERER,
                ],
            )
            .await
            .context("INSERT xdp_rate_rtt_config")?;
        let config_id = pg_dsn::config_id_from_row(&cfg)?;
        for chunk in samples.chunks(1024) {
            let ids = vec![config_id; chunk.len()];
            let t0s: Vec<_> = chunk.iter().map(|r| r.t0).collect();
            let seqs: Vec<_> = chunk.iter().map(|r| r.seq).collect();
            let oks: Vec<_> = chunk.iter().map(|r| r.ok).collect();
            let rtts: Vec<_> = chunk.iter().map(|r| r.rtt_ms).collect();
            tx.execute(
                "INSERT INTO xdp_rate_rtt_data (config_id,t0,seq,ok,rtt_ms) \
                 SELECT * FROM UNNEST($1::bigint[],$2::timestamptz[],$3::int4[],$4::bool[],$5::float8[])",
                &[&ids, &t0s, &seqs, &oks, &rtts],
            )
            .await
            .context("INSERT xdp_rate_rtt_data")?;
        }
        tx.commit().await.context("commit")?;
        Ok(config_id)
    }
}
