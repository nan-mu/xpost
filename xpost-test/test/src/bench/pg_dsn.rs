//! Shared Postgres DSN: always `dbname=postgres`.
//! `--formal` → schema `formal` (CREATE IF NOT EXISTS + search_path); else `public`.

use anyhow::{Context, Result};
use log::info;
use tokio_postgres::{Client, NoTls};

const HOST: &str = "192.168.110.96";
const PORT: u16 = 5432;
const USER: &str = "nan";
const PASS: &str = "22332233";
const DBNAME: &str = "postgres";
const FORMAL_SCHEMA: &str = "formal";

#[derive(Clone, Copy, Debug, Default)]
pub struct PgOpts {
    pub formal: bool,
}

impl PgOpts {
    pub fn schema(self) -> &'static str {
        if self.formal { FORMAL_SCHEMA } else { "public" }
    }

    pub fn dsn(self) -> String {
        format!(
            "host={HOST} port={PORT} dbname={DBNAME} user={USER} password={PASS} connect_timeout=10 sslmode=disable"
        )
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
    /// HAL vs XDP PPS → `experiment_runs` / `counter_samples`.
    ExperimentPps,
    /// just_tx RTT at ~5% loss operating point → `just_tx_rtt_*`.
    JustTxRtt,
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
            Self::ExperimentPps => {
                "TRUNCATE TABLE sample_reads, counter_samples, experiment_runs RESTART IDENTITY CASCADE"
            }
            Self::JustTxRtt => {
                "TRUNCATE TABLE just_tx_rtt_samples, just_tx_rtt_runs RESTART IDENTITY CASCADE"
            }
        }
    }

    /// `formal` only: drop tables so `ensure_sql` can recreate current schema (no migrations).
    fn recreate_sql(self) -> Option<&'static str> {
        match self {
            Self::Drop => {
                Some("DROP TABLE IF EXISTS xdp_drop_rxcnt, xdp_drop_round, xdp_drop_config CASCADE")
            }
            Self::ExperimentPps => Some(
                "DROP TABLE IF EXISTS sample_reads, counter_samples, experiment_runs CASCADE",
            ),
            Self::JustTxRtt => Some(
                "DROP TABLE IF EXISTS just_tx_rtt_samples, just_tx_rtt_runs CASCADE",
            ),
            _ => None,
        }
    }

    fn ensure_sql(self) -> &'static [&'static str] {
        match self {
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
            Self::ExperimentPps => &[
                r#"CREATE TABLE IF NOT EXISTS experiment_runs (
                    run_id UUID PRIMARY KEY,
                    pair_id INTEGER NOT NULL,
                    run_kind TEXT NOT NULL CHECK (run_kind IN ('random', 'control', 'pilot')),
                    variant TEXT NOT NULL CHECK (variant IN ('hal', 'xdp')),
                    firmware_sha256 TEXT,
                    elf_sha256 TEXT,
                    sender_target_pps BIGINT,
                    udp_payload_bytes INTEGER NOT NULL DEFAULT 8,
                    sample_seed BIGINT,
                    window_secs DOUBLE PRECISION,
                    sample_interval_ms INTEGER,
                    started_at_utc TIMESTAMPTZ,
                    stopped_at_utc TIMESTAMPTZ,
                    status TEXT NOT NULL,
                    status_reason TEXT,
                    source_note TEXT
                )"#,
                r#"CREATE TABLE IF NOT EXISTS counter_samples (
                    run_id UUID NOT NULL REFERENCES experiment_runs(run_id) ON DELETE CASCADE,
                    time_ns BIGINT NOT NULL,
                    counter_value BIGINT NOT NULL,
                    PRIMARY KEY (run_id, time_ns)
                )"#,
                r#"CREATE TABLE IF NOT EXISTS sample_reads (
                    run_id UUID NOT NULL REFERENCES experiment_runs(run_id) ON DELETE CASCADE,
                    sample_id INTEGER NOT NULL,
                    triad_id INTEGER,
                    tau_s DOUBLE PRECISION,
                    t_req_ns BIGINT,
                    t_resp_ns BIGINT,
                    counter_value BIGINT,
                    valid BOOLEAN NOT NULL DEFAULT TRUE,
                    PRIMARY KEY (run_id, sample_id)
                )"#,
                r#"CREATE INDEX IF NOT EXISTS counter_samples_run_time
                    ON counter_samples (run_id, time_ns)"#,
            ],
            Self::JustTxRtt => &[
                r#"CREATE TABLE IF NOT EXISTS just_tx_rtt_runs (
                    run_id UUID PRIMARY KEY,
                    pair_id INTEGER NOT NULL,
                    run_kind TEXT NOT NULL CHECK (run_kind IN ('random', 'control', 'pilot')),
                    variant TEXT NOT NULL CHECK (variant IN ('hal', 'xdp')),
                    phase TEXT NOT NULL DEFAULT 'operating'
                        CHECK (phase IN ('sweep', 'operating')),
                    target_pps DOUBLE PRECISION NOT NULL,
                    calibrated_loss DOUBLE PRECISION,
                    window_secs DOUBLE PRECISION,
                    sent BIGINT NOT NULL,
                    ok BIGINT NOT NULL,
                    loss_rate DOUBLE PRECISION NOT NULL,
                    return_pps DOUBLE PRECISION,
                    device_rxcnt_delta BIGINT,
                    median_rtt_ms DOUBLE PRECISION,
                    p95_rtt_ms DOUBLE PRECISION,
                    mean_rtt_ms DOUBLE PRECISION,
                    started_at_utc TIMESTAMPTZ,
                    stopped_at_utc TIMESTAMPTZ,
                    status TEXT NOT NULL,
                    source_note TEXT
                )"#,
                r#"ALTER TABLE just_tx_rtt_runs
                    ADD COLUMN IF NOT EXISTS phase TEXT NOT NULL DEFAULT 'operating'"#,
                r#"CREATE TABLE IF NOT EXISTS just_tx_rtt_samples (
                    run_id UUID NOT NULL REFERENCES just_tx_rtt_runs(run_id) ON DELETE CASCADE,
                    seq INTEGER NOT NULL,
                    t0 TIMESTAMPTZ NOT NULL,
                    ok BOOLEAN NOT NULL,
                    rtt_ms DOUBLE PRECISION,
                    PRIMARY KEY (run_id, seq)
                )"#,
                r#"CREATE INDEX IF NOT EXISTS just_tx_rtt_samples_run
                    ON just_tx_rtt_samples (run_id)"#,
            ],
        }
    }
}

pub async fn connect(opts: PgOpts) -> Result<Client> {
    let dsn = opts.dsn();
    let (client, conn) = tokio_postgres::connect(&dsn, NoTls)
        .await
        .with_context(|| format!("postgres connect db={DBNAME}"))?;
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            log::warn!("postgres connection closed: {e}");
        }
    });
    info!(
        "postgres connected {HOST}:{PORT} db={DBNAME} schema={} user={USER} formal={}",
        opts.schema(),
        opts.formal
    );
    Ok(client)
}

pub async fn prepare(client: &Client, opts: PgOpts, set: TableSet) -> Result<()> {
    if opts.formal {
        client
            .batch_execute(&format!("CREATE SCHEMA IF NOT EXISTS {FORMAL_SCHEMA}"))
            .await
            .context("CREATE SCHEMA formal")?;
        client
            .batch_execute(&format!("SET search_path TO {FORMAL_SCHEMA}"))
            .await
            .context("SET search_path formal")?;
        if let Some(sql) = set.recreate_sql() {
            client
                .batch_execute(sql)
                .await
                .with_context(|| format!("recreate tables {:?} in {FORMAL_SCHEMA}", set))?;
            info!("formal schema={FORMAL_SCHEMA}: recreated {:?} tables", set);
        }
    } else {
        client
            .batch_execute("SET search_path TO public")
            .await
            .context("SET search_path public")?;
    }

    for sql in set.ensure_sql() {
        client
            .batch_execute(sql)
            .await
            .with_context(|| format!("ensure tables {:?} in {}", set, opts.schema()))?;
    }
    if opts.formal && set.recreate_sql().is_none() {
        client
            .batch_execute(set.truncate_sql())
            .await
            .with_context(|| format!("TRUNCATE {:?} in {FORMAL_SCHEMA}", set))?;
        info!("formal schema={FORMAL_SCHEMA}: truncated {:?}", set);
    }
    Ok(())
}

/// Ensure `just_tx_rtt_*` exist; on formal, delete only rows with `source_note = note`.
pub async fn prepare_just_tx_rtt_note(client: &Client, opts: PgOpts, note: &str) -> Result<()> {
    if opts.formal {
        client
            .batch_execute(&format!("CREATE SCHEMA IF NOT EXISTS {FORMAL_SCHEMA}"))
            .await
            .context("CREATE SCHEMA formal")?;
        client
            .batch_execute(&format!("SET search_path TO {FORMAL_SCHEMA}"))
            .await
            .context("SET search_path formal")?;
    } else {
        client
            .batch_execute("SET search_path TO public")
            .await
            .context("SET search_path public")?;
    }

    for sql in TableSet::JustTxRtt.ensure_sql() {
        client
            .batch_execute(sql)
            .await
            .with_context(|| format!("ensure JustTxRtt tables in {}", opts.schema()))?;
    }

    if opts.formal {
        let n = client
            .execute(
                "DELETE FROM just_tx_rtt_runs WHERE source_note = $1",
                &[&note],
            )
            .await
            .context("DELETE just_tx_rtt_runs by source_note")?;
        info!(
            "formal schema={FORMAL_SCHEMA}: deleted {n} just_tx_rtt_runs with source_note={note}"
        );
    }
    Ok(())
}

/// Ensure `experiment_*` exist; on formal, delete only rows with `source_note = note`
/// (does not DROP/TRUNCATE the whole PPS table family).
pub async fn prepare_experiment_pps_note(client: &Client, opts: PgOpts, note: &str) -> Result<()> {
    if opts.formal {
        client
            .batch_execute(&format!("CREATE SCHEMA IF NOT EXISTS {FORMAL_SCHEMA}"))
            .await
            .context("CREATE SCHEMA formal")?;
        client
            .batch_execute(&format!("SET search_path TO {FORMAL_SCHEMA}"))
            .await
            .context("SET search_path formal")?;
    } else {
        client
            .batch_execute("SET search_path TO public")
            .await
            .context("SET search_path public")?;
    }

    for sql in TableSet::ExperimentPps.ensure_sql() {
        client
            .batch_execute(sql)
            .await
            .with_context(|| {
                format!(
                    "ensure ExperimentPps tables in {}",
                    opts.schema()
                )
            })?;
    }

    if opts.formal {
        // CASCADE from experiment_runs clears counter_samples / sample_reads.
        let n = client
            .execute(
                "DELETE FROM experiment_runs WHERE source_note = $1",
                &[&note],
            )
            .await
            .context("DELETE experiment_runs by source_note")?;
        info!(
            "formal schema={FORMAL_SCHEMA}: deleted {n} experiment_runs with source_note={note}"
        );
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
