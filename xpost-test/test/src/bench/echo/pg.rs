//! Postgres ingest for echo baseline (`xdp_echo_*`).

use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use log::warn;
use tokio_postgres::Client;

use crate::bench::pg_dsn::{self, PgOpts, TableSet};
use crate::bench::stop::Stop;

const INSERT_CONFIG: &str = "INSERT INTO xdp_echo_config \
    (interval_us, udp_payload_len, window_s, probe_timeout_ms) \
    VALUES ($1, $2, $3, $4) RETURNING id";

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
    pub async fn connect(opts: PgOpts) -> Result<Self> {
        let client = pg_dsn::connect(opts).await?;
        pg_dsn::prepare(&client, opts, TableSet::Echo).await?;
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
        udp_payload_len: i32,
        window_s: i32,
        probe_timeout_ms: i32,
        rtts: &[ProbeRtt],
        stop: &Stop,
    ) -> Option<i64> {
        let mut n = 0u32;
        loop {
            match self
                .ingest_once(
                    interval_us,
                    udp_payload_len,
                    window_s,
                    probe_timeout_ms,
                    rtts,
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
        udp_payload_len: i32,
        window_s: i32,
        probe_timeout_ms: i32,
        rtts: &[ProbeRtt],
    ) -> Result<i64> {
        let tx = self.client.transaction().await.context("begin")?;
        let cfg = tx
            .query_one(
                INSERT_CONFIG,
                &[&interval_us, &udp_payload_len, &window_s, &probe_timeout_ms],
            )
            .await
            .context("INSERT xdp_echo_config")?;
        let config_id = pg_dsn::config_id_from_row(&cfg)?;
        for chunk in rtts.chunks(1024) {
            let t0s: Vec<SystemTime> = chunk.iter().map(|r| r.t0).collect();
            let seqs: Vec<i32> = chunk.iter().map(|r| r.seq).collect();
            let oks: Vec<bool> = chunk.iter().map(|r| r.ok).collect();
            let rtt_ms: Vec<Option<f64>> = chunk.iter().map(|r| r.rtt_ms).collect();
            let ids: Vec<i64> = vec![config_id; chunk.len()];
            tx.execute(
                "INSERT INTO xdp_echo_data (config_id, t0, seq, ok, rtt_ms) \
                 SELECT * FROM UNNEST($1::bigint[], $2::timestamptz[], $3::int4[], $4::bool[], $5::float8[])",
                &[&ids, &t0s, &seqs, &oks, &rtt_ms],
            )
            .await
            .context("INSERT xdp_echo_data")?;
        }
        tx.commit().await.context("commit")?;
        Ok(config_id)
    }
}
