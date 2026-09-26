"""CREATE IF NOT EXISTS for experiment_* and just_tx_rtt_* tables."""

from __future__ import annotations

from utils.db import connect, dsn_summary, schema

DDL = [
    """
    CREATE TABLE IF NOT EXISTS experiment_runs (
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
    )
    """,
    """
    CREATE TABLE IF NOT EXISTS counter_samples (
        run_id UUID NOT NULL REFERENCES experiment_runs(run_id) ON DELETE CASCADE,
        time_ns BIGINT NOT NULL,
        counter_value BIGINT NOT NULL,
        PRIMARY KEY (run_id, time_ns)
    )
    """,
    """
    CREATE TABLE IF NOT EXISTS sample_reads (
        run_id UUID NOT NULL REFERENCES experiment_runs(run_id) ON DELETE CASCADE,
        sample_id INTEGER NOT NULL,
        triad_id INTEGER,
        tau_s DOUBLE PRECISION,
        t_req_ns BIGINT,
        t_resp_ns BIGINT,
        counter_value BIGINT,
        valid BOOLEAN NOT NULL DEFAULT TRUE,
        PRIMARY KEY (run_id, sample_id)
    )
    """,
    """
    CREATE INDEX IF NOT EXISTS counter_samples_run_time
        ON counter_samples (run_id, time_ns)
    """,
    """
    CREATE TABLE IF NOT EXISTS just_tx_rtt_runs (
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
    )
    """,
    """
    ALTER TABLE just_tx_rtt_runs
        ADD COLUMN IF NOT EXISTS phase TEXT NOT NULL DEFAULT 'operating'
    """,
    """
    CREATE TABLE IF NOT EXISTS just_tx_rtt_samples (
        run_id UUID NOT NULL REFERENCES just_tx_rtt_runs(run_id) ON DELETE CASCADE,
        seq INTEGER NOT NULL,
        t0 TIMESTAMPTZ NOT NULL,
        ok BOOLEAN NOT NULL,
        rtt_ms DOUBLE PRECISION,
        PRIMARY KEY (run_id, seq)
    )
    """,
    """
    CREATE INDEX IF NOT EXISTS just_tx_rtt_samples_run
        ON just_tx_rtt_samples (run_id)
    """,
]


def main() -> None:
    print(f"ensure-schema → {dsn_summary()}")
    with connect() as conn:
        sch = schema()
        if sch != "public":
            conn.execute(f"CREATE SCHEMA IF NOT EXISTS {sch}")
        for stmt in DDL:
            conn.execute(stmt)
        conn.commit()
    print("ok: experiment_* / just_tx_rtt_*")


if __name__ == "__main__":
    main()
