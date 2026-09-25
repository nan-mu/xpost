"""CREATE IF NOT EXISTS for just_drop_pps_* tables."""

from __future__ import annotations

from utils.db import connect, dsn_summary, schema

DDL = [
    """
    CREATE TABLE IF NOT EXISTS just_drop_pps_run (
        id bigserial PRIMARY KEY,
        created_at timestamptz NOT NULL DEFAULT now(),
        path text NOT NULL CHECK (path IN ('hal', 'xdp')),
        window_secs integer NOT NULL CHECK (window_secs > 0),
        sample_interval_ms integer NOT NULL CHECK (sample_interval_ms > 0),
        note text
    )
    """,
    """
    CREATE TABLE IF NOT EXISTS just_drop_pps_round (
        id bigserial PRIMARY KEY,
        run_id bigint NOT NULL REFERENCES just_drop_pps_run (id) ON DELETE CASCADE,
        round integer NOT NULL CHECK (round > 0),
        overall_pps double precision NOT NULL,
        interval_pps_mean double precision,
        interval_pps_std double precision,
        host_pps double precision,
        drop_delta bigint,
        net_rx_delta bigint,
        elapsed_secs double precision,
        UNIQUE (run_id, round)
    )
    """,
    """
    CREATE TABLE IF NOT EXISTS just_drop_pps_sample (
        id bigserial PRIMARY KEY,
        round_id bigint NOT NULL REFERENCES just_drop_pps_round (id) ON DELETE CASCADE,
        t_s double precision NOT NULL,
        drop_count bigint NOT NULL,
        net_rx bigint NOT NULL,
        host_tx_end bigint NOT NULL DEFAULT 0,
        interval_pps double precision,
        overall_pps_so_far double precision
    )
    """,
    """
    CREATE INDEX IF NOT EXISTS just_drop_pps_sample_round_t
        ON just_drop_pps_sample (round_id, t_s)
    """,
]


def main() -> None:
    print(f"ensure-schema → {dsn_summary()}")
    with connect() as conn:
        # formal schema may need CREATE SCHEMA
        sch = schema()
        if sch != "public":
            conn.execute(f"CREATE SCHEMA IF NOT EXISTS {sch}")
        for stmt in DDL:
            conn.execute(stmt)
        conn.commit()
    print("ok: just_drop_pps_run / round / sample")


if __name__ == "__main__":
    main()
