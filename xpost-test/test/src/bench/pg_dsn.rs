//! Shared Postgres DSN from compile-time `config/default.toml` `[postgres]`.
//! `formal` → `[postgres].formal_schema` (CREATE IF NOT EXISTS + search_path); else `public`.

use anyhow::{Context, Result};
use log::info;
use tokio_postgres::{Client, NoTls};

use crate::config;

#[derive(Clone, Copy, Debug, Default)]
pub struct PgOpts {
    pub formal: bool,
}

impl PgOpts {
    pub fn schema(self) -> &'static str {
        if self.formal {
            config::postgres().formal_schema.as_str()
        } else {
            "public"
        }
    }

    pub fn dsn(self) -> String {
        config::postgres().dsn()
    }
}

#[derive(Clone, Copy, Debug)]
pub enum TableSet {
    Echo,
    Drop,
    WlRtt,
    WlLoss,
    RateRtt,
    RateLoss,
    Ack,
    /// Formal `just_tx` → use [`crate::bench::formal`] (exam schema; no DROP).
    JustTx,
    /// Formal `just_drop` → use [`crate::bench::formal`] (exam schema; no DROP).
    JustDrop,
    /// Debug `dev` → console-only (never formal-DROP).
    Dev,
}

impl TableSet {
    fn truncate_sql(self) -> &'static str {
        match self {
            Self::Echo => "TRUNCATE TABLE xdp_echo_data, xdp_echo_config RESTART IDENTITY CASCADE",
            Self::Drop => {
                "TRUNCATE TABLE xdp_drop_rxcnt, xdp_drop_round, xdp_drop_config RESTART IDENTITY CASCADE"
            }
            Self::WlRtt => "TRUNCATE TABLE xdp_ddos_data, xdp_ddos_config RESTART IDENTITY CASCADE",
            Self::WlLoss => {
                "TRUNCATE TABLE xdp_loss_data, xdp_loss_config RESTART IDENTITY CASCADE"
            }
            Self::RateRtt => {
                "TRUNCATE TABLE xdp_rate_rtt_data, xdp_rate_rtt_config RESTART IDENTITY CASCADE"
            }
            Self::RateLoss => {
                "TRUNCATE TABLE xdp_rate_loss_data, xdp_rate_loss_config RESTART IDENTITY CASCADE"
            }
            Self::Ack => {
                "TRUNCATE TABLE xdp_ack_trial, xdp_ack_config RESTART IDENTITY CASCADE"
            }
            // Formal just_* uses exam CASCADE wipe via `formal::clear` only.
            Self::JustTx | Self::JustDrop | Self::Dev => "",
        }
    }

    /// `formal` only: drop same-named tables so `ensure_*` recreates empty current schema.
    /// Formal just_tx / just_drop never DROP on open-run (see `formal::ensure` / `formal::clear`).
    /// `Dev` never recreates — console-only debug, no tables.
    fn recreate_sql(self) -> Option<&'static str> {
        match self {
            Self::Drop => {
                Some("DROP TABLE IF EXISTS xdp_drop_rxcnt, xdp_drop_round, xdp_drop_config CASCADE")
            }
            Self::JustTx | Self::JustDrop | Self::Dev => None,
            _ => None,
        }
    }

    /// Legacy just_tx / just_drop DDL retired — formal path uses `sql/formal.sql`.
    fn ensure_batch(self) -> Option<&'static str> {
        match self {
            Self::JustTx | Self::JustDrop | Self::Dev => None,
            _ => None,
        }
    }

    fn ensure_sql(self) -> &'static [&'static str] {
        match self {
            Self::JustTx | Self::JustDrop | Self::Dev => &[],
            Self::Echo => &[
                r#"CREATE TABLE IF NOT EXISTS xdp_echo_config (
                    id bigserial PRIMARY KEY,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    interval_us integer NOT NULL CHECK (interval_us >= 0),
                    udp_payload_len integer NOT NULL CHECK (udp_payload_len > 0),
                    window_s integer NOT NULL CHECK (window_s > 0),
                    probe_timeout_ms integer NOT NULL CHECK (probe_timeout_ms > 0)
                )"#,
                r#"CREATE TABLE IF NOT EXISTS xdp_echo_data (
                    id bigserial PRIMARY KEY,
                    config_id bigint NOT NULL REFERENCES xdp_echo_config (id) ON DELETE CASCADE,
                    t0 timestamptz NOT NULL,
                    seq integer NOT NULL CHECK (seq >= 0),
                    ok boolean NOT NULL,
                    rtt_ms double precision
                )"#,
            ],
            Self::Drop => &[
                r#"CREATE TABLE IF NOT EXISTS xdp_drop_config (
                    id bigserial PRIMARY KEY,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    path text NOT NULL CHECK (path IN ('xdp_drop', 'hal_drop')),
                    window_s integer NOT NULL CHECK (window_s > 0),
                    interferer text NOT NULL DEFAULT 'raw_udp',
                    flood boolean NOT NULL DEFAULT true,
                    hping_pps integer CHECK (hping_pps IS NULL OR hping_pps >= 0),
                    hping_args text,
                    eth_type text NOT NULL DEFAULT 'ipv4'
                        CHECK (eth_type IN ('ipv4', 'ipv6', 'other')),
                    ip_proto integer NOT NULL DEFAULT 17
                        CHECK (ip_proto >= 0 AND ip_proto <= 255),
                    udp_dport integer CHECK (udp_dport IS NULL OR (udp_dport > 0 AND udp_dport < 65536)),
                    note text
                )"#,
                r#"CREATE TABLE IF NOT EXISTS xdp_drop_round (
                    id bigserial PRIMARY KEY,
                    config_id bigint NOT NULL REFERENCES xdp_drop_config (id) ON DELETE CASCADE,
                    t0 timestamptz NOT NULL DEFAULT now(),
                    elapsed_s double precision NOT NULL CHECK (elapsed_s > 0),
                    tx_start_ts timestamptz NOT NULL,
                    tx_end_ts timestamptz NOT NULL,
                    tx_count bigint NOT NULL CHECK (tx_count >= 0),
                    host_pps double precision NOT NULL CHECK (host_pps >= 0),
                    dut_pps double precision,
                    pkts_drop bigint NOT NULL CHECK (pkts_drop >= 0),
                    map_dump_ok boolean NOT NULL DEFAULT true,
                    t_first_us bigint NOT NULL DEFAULT 0 CHECK (t_first_us >= 0),
                    t_last_us bigint NOT NULL DEFAULT 0 CHECK (t_last_us >= 0)
                )"#,
                r#"CREATE TABLE IF NOT EXISTS xdp_drop_rxcnt (
                    id bigserial PRIMARY KEY,
                    config_id bigint NOT NULL REFERENCES xdp_drop_config (id) ON DELETE CASCADE,
                    round_id bigint NOT NULL REFERENCES xdp_drop_round (id) ON DELETE CASCADE,
                    ip_proto integer NOT NULL CHECK (ip_proto >= 0 AND ip_proto <= 255),
                    pkt_count bigint NOT NULL CHECK (pkt_count >= 0)
                )"#,
            ],
            Self::WlRtt => &[
                r#"CREATE TABLE IF NOT EXISTS xdp_ddos_config (
                    id bigserial PRIMARY KEY,
                    interval_us integer NOT NULL,
                    udp_payload_len integer NOT NULL,
                    trim text NOT NULL CHECK (trim = 'ddos'),
                    u_pct integer NOT NULL CHECK (u_pct >= 0)
                )"#,
                r#"CREATE TABLE IF NOT EXISTS xdp_ddos_data (
                    id bigserial PRIMARY KEY,
                    t0 timestamptz NOT NULL,
                    rtt_ms double precision,
                    reply text NOT NULL CHECK (reply IN ('U', 'NU', 'N', 'FP')),
                    config_id bigint NOT NULL REFERENCES xdp_ddos_config (id)
                )"#,
            ],
            Self::WlLoss => &[
                r#"CREATE TABLE IF NOT EXISTS xdp_loss_config (
                    id bigserial PRIMARY KEY,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    interval_us integer NOT NULL CHECK (interval_us >= 0),
                    stream_len integer NOT NULL CHECK (stream_len > 0),
                    udp_payload_len integer NOT NULL CHECK (udp_payload_len > 0),
                    hping_pps integer NOT NULL CHECK (hping_pps >= 0),
                    window_s integer NOT NULL CHECK (window_s > 0),
                    interferer text NOT NULL DEFAULT 'hping3',
                    stream_seed bigint NOT NULL
                )"#,
                r#"CREATE TABLE IF NOT EXISTS xdp_loss_data (
                    id bigserial PRIMARY KEY,
                    config_id bigint NOT NULL REFERENCES xdp_loss_config (id),
                    t0 timestamptz NOT NULL DEFAULT now(),
                    sent_pkts integer NOT NULL CHECK (sent_pkts >= 0),
                    sent_bytes integer NOT NULL CHECK (sent_bytes >= 0),
                    recv_bytes integer NOT NULL CHECK (recv_bytes >= 0),
                    match_bytes integer NOT NULL CHECK (match_bytes >= 0),
                    loss_pct double precision CHECK (loss_pct IS NULL OR (loss_pct >= 0 AND loss_pct <= 100)),
                    dump_ok boolean NOT NULL DEFAULT true,
                    dump_attempts integer NOT NULL DEFAULT 1 CHECK (dump_attempts >= 1),
                    sent_sha256 text,
                    recv_sha256 text
                )"#,
            ],
            Self::RateRtt => &[
                r#"CREATE TABLE IF NOT EXISTS xdp_rate_rtt_config (
                    id bigserial PRIMARY KEY,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    attack_pps integer NOT NULL CHECK (attack_pps >= 0),
                    peak_pct integer NOT NULL CHECK (peak_pct >= 0),
                    p_peak integer NOT NULL CHECK (p_peak > 0),
                    thresh integer NOT NULL CHECK (thresh > 0),
                    mitigation_on boolean NOT NULL,
                    window_s integer NOT NULL CHECK (window_s > 0),
                    probe_interval_ms integer NOT NULL CHECK (probe_interval_ms > 0),
                    cell_repeat integer NOT NULL DEFAULT 1 CHECK (cell_repeat >= 1),
                    interferer text NOT NULL DEFAULT 'hping3'
                )"#,
                r#"CREATE TABLE IF NOT EXISTS xdp_rate_rtt_data (
                    id bigserial PRIMARY KEY,
                    config_id bigint NOT NULL REFERENCES xdp_rate_rtt_config (id) ON DELETE CASCADE,
                    t0 timestamptz NOT NULL,
                    seq integer NOT NULL CHECK (seq >= 0),
                    ok boolean NOT NULL,
                    rtt_ms double precision
                )"#,
            ],
            Self::RateLoss => &[
                r#"CREATE TABLE IF NOT EXISTS xdp_rate_loss_config (
                    id bigserial PRIMARY KEY,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    attack_pps integer NOT NULL CHECK (attack_pps >= 0),
                    peak_pct integer NOT NULL CHECK (peak_pct >= 0),
                    p_peak integer NOT NULL CHECK (p_peak > 0),
                    thresh integer NOT NULL CHECK (thresh > 0),
                    mitigation_on boolean NOT NULL,
                    interval_us integer NOT NULL CHECK (interval_us >= 0),
                    stream_len integer NOT NULL CHECK (stream_len > 0),
                    udp_payload_len integer NOT NULL CHECK (udp_payload_len > 0),
                    window_s integer NOT NULL CHECK (window_s > 0),
                    interferer text NOT NULL DEFAULT 'hping3',
                    stream_seed bigint NOT NULL,
                    cell_repeat integer NOT NULL DEFAULT 1 CHECK (cell_repeat >= 1)
                )"#,
                r#"CREATE TABLE IF NOT EXISTS xdp_rate_loss_data (
                    id bigserial PRIMARY KEY,
                    config_id bigint NOT NULL REFERENCES xdp_rate_loss_config (id) ON DELETE CASCADE,
                    t0 timestamptz NOT NULL DEFAULT now(),
                    sent_pkts integer NOT NULL CHECK (sent_pkts >= 0),
                    sent_bytes integer NOT NULL CHECK (sent_bytes >= 0),
                    recv_bytes integer NOT NULL CHECK (recv_bytes >= 0),
                    match_bytes integer NOT NULL CHECK (match_bytes >= 0),
                    loss_pct double precision CHECK (loss_pct IS NULL OR (loss_pct >= 0 AND loss_pct <= 100)),
                    dump_ok boolean NOT NULL DEFAULT true,
                    dump_attempts integer NOT NULL DEFAULT 1 CHECK (dump_attempts >= 1),
                    sent_sha256 text,
                    recv_sha256 text
                )"#,
            ],
            Self::Ack => &[
                r#"CREATE TABLE IF NOT EXISTS xdp_ack_config (
                    id bigserial PRIMARY KEY,
                    created_at timestamptz NOT NULL DEFAULT now(),
                    loaded text NOT NULL CHECK (loaded IN ('none', 'drop')),
                    stim_port integer NOT NULL CHECK (stim_port > 0 AND stim_port < 65536),
                    stim_n integer NOT NULL DEFAULT 1 CHECK (stim_n > 0)
                )"#,
                r#"CREATE TABLE IF NOT EXISTS xdp_ack_trial (
                    id bigserial PRIMARY KEY,
                    config_id bigint NOT NULL REFERENCES xdp_ack_config (id) ON DELETE CASCADE,
                    expect_ack boolean NOT NULL,
                    got_ack boolean NOT NULL,
                    ack_count integer NOT NULL DEFAULT 0 CHECK (ack_count >= 0),
                    drop_before bigint NOT NULL CHECK (drop_before >= 0),
                    drop_after bigint NOT NULL CHECK (drop_after >= 0),
                    net_rx_before bigint NOT NULL CHECK (net_rx_before >= 0),
                    net_rx_after bigint NOT NULL CHECK (net_rx_after >= 0),
                    pass boolean NOT NULL
                )"#,
                r#"ALTER TABLE xdp_ack_config ADD COLUMN IF NOT EXISTS stim_n integer NOT NULL DEFAULT 1"#,
                r#"ALTER TABLE xdp_ack_trial ADD COLUMN IF NOT EXISTS ack_count integer NOT NULL DEFAULT 0"#,
            ],
        }
    }

    async fn apply_ensure(self, client: &Client, schema: &str) -> Result<()> {
        if let Some(batch) = self.ensure_batch() {
            client
                .batch_execute(batch)
                .await
                .with_context(|| format!("ensure {:?} DDL batch in {schema}", self))?;
            return Ok(());
        }
        for sql in self.ensure_sql() {
            client
                .batch_execute(sql)
                .await
                .with_context(|| format!("ensure tables {:?} in {schema}", self))?;
        }
        Ok(())
    }
}

pub async fn connect(opts: PgOpts) -> Result<Client> {
    let pg = config::postgres();
    let dsn = opts.dsn();
    let (client, conn) = tokio_postgres::connect(&dsn, NoTls)
        .await
        .with_context(|| format!("postgres connect db={}", pg.dbname))?;
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            log::warn!("postgres connection closed: {e}");
        }
    });
    info!(
        "postgres connected {}:{} db={} schema={} user={} formal={}",
        pg.host,
        pg.port,
        pg.dbname,
        opts.schema(),
        pg.user,
        opts.formal
    );
    Ok(client)
}

pub async fn prepare(client: &Client, opts: PgOpts, set: TableSet) -> Result<()> {
    let formal_schema = config::postgres().formal_schema.as_str();
    if opts.formal {
        client
            .batch_execute(&format!("CREATE SCHEMA IF NOT EXISTS {formal_schema}"))
            .await
            .context("CREATE SCHEMA formal")?;
        client
            .batch_execute(&format!("SET search_path TO {formal_schema}"))
            .await
            .context("SET search_path formal")?;
        if let Some(sql) = set.recreate_sql() {
            client
                .batch_execute(sql)
                .await
                .with_context(|| format!("recreate tables {:?} in {formal_schema}", set))?;
            info!("formal schema={formal_schema}: recreated {:?} tables", set);
        }
    } else {
        client
            .batch_execute("SET search_path TO public")
            .await
            .context("SET search_path public")?;
    }

    set.apply_ensure(client, opts.schema()).await?;
    if opts.formal && set.recreate_sql().is_none() {
        client
            .batch_execute(set.truncate_sql())
            .await
            .with_context(|| format!("TRUNCATE {:?} in {formal_schema}", set))?;
        info!("formal schema={formal_schema}: truncated {:?}", set);
    }
    Ok(())
}

pub fn config_id_from_row(row: &tokio_postgres::Row) -> Result<i64> {
    if let Ok(v) = row.try_get::<_, i64>(0) {
        return Ok(v);
    }
    if let Ok(v) = row.try_get::<_, i32>(0) {
        return Ok(v as i64);
    }
    anyhow::bail!("config id is neither i32 nor i64")
}
