"""CREATE IF NOT EXISTS for experiment_* tables (PPS contract)."""

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
    print("ok: experiment_runs / counter_samples / sample_reads")


if __name__ == "__main__":
    main()
