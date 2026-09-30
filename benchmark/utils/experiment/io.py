"""Load experiment / just_tx tables into DataFrames (live Postgres)."""

from __future__ import annotations

import pandas as pd

from utils.db import read_sql

# Sweep windows are ~8–10 s; operating ~20 s. Split without a phase column.
_JUST_TX_OP_DURATION_S = 15.0


def load_runs(
    *,
    run_kind: str | None = None,
    status: str = "complete",
    source_note: str | None = None,
) -> pd.DataFrame:
    clauses = ["status = %(status)s"]
    params: dict = {"status": status}
    if run_kind is not None:
        clauses.append("run_kind = %(run_kind)s")
        params["run_kind"] = run_kind
    if source_note is not None:
        clauses.append("source_note = %(source_note)s")
        params["source_note"] = source_note
    where = " AND ".join(clauses)
    return read_sql(
        f"""
        SELECT run_id::text AS run_id, pair_id, run_kind, variant,
               firmware_sha256, elf_sha256, sender_target_pps,
               sample_seed, window_secs, sample_interval_ms,
               started_at_utc, stopped_at_utc, status, status_reason, source_note
        FROM experiment_runs
        WHERE {where}
        ORDER BY variant, pair_id, started_at_utc
        """,
        params,
    )


def load_just_tx_config(*, fw_type: str | None = None) -> pd.DataFrame:
    """Windows from ``just_tx_config`` with derived ``duration_s`` / ``phase``."""
    clauses: list[str] = []
    params: dict = {"op_s": _JUST_TX_OP_DURATION_S}
    if fw_type is not None:
        clauses.append("fw_type = %(fw_type)s")
        params["fw_type"] = fw_type
    where = f"WHERE {' AND '.join(clauses)}" if clauses else ""
    return read_sql(
        f"""
        SELECT id AS config_id, fw_type, target_pps, started_at, stopped_at, sent, counters,
               EXTRACT(EPOCH FROM (stopped_at - started_at))::double precision AS duration_s,
               CASE
                 WHEN EXTRACT(EPOCH FROM (stopped_at - started_at)) >= %(op_s)s
                 THEN 'operating'
                 ELSE 'sweep'
               END AS phase
        FROM just_tx_config
        {where}
        ORDER BY fw_type, target_pps, started_at
        """,
        params,
    )


def load_just_tx_window_stats(
    *,
    phase: str | None = None,
    fw_type: str | None = None,
) -> pd.DataFrame:
    """Per-window ok / loss / return_pps from ``just_tx_config`` + ``just_tx_data``."""
    clauses: list[str] = []
    params: dict = {"op_s": _JUST_TX_OP_DURATION_S}
    if fw_type is not None:
        clauses.append("c.fw_type = %(fw_type)s")
        params["fw_type"] = fw_type
    if phase is not None:
        if phase == "operating":
            clauses.append(
                "EXTRACT(EPOCH FROM (c.stopped_at - c.started_at)) >= %(op_s)s"
            )
        elif phase == "sweep":
            clauses.append(
                "EXTRACT(EPOCH FROM (c.stopped_at - c.started_at)) < %(op_s)s"
            )
        else:
            raise ValueError(f"phase must be 'sweep' or 'operating', got {phase!r}")
    where = f"WHERE {' AND '.join(clauses)}" if clauses else ""
    return read_sql(
        f"""
        SELECT c.id AS config_id, c.fw_type, c.target_pps, c.started_at, c.stopped_at,
               c.sent, c.counters,
               EXTRACT(EPOCH FROM (c.stopped_at - c.started_at))::double precision AS duration_s,
               CASE
                 WHEN EXTRACT(EPOCH FROM (c.stopped_at - c.started_at)) >= %(op_s)s
                 THEN 'operating'
                 ELSE 'sweep'
               END AS phase,
               COALESCE(d.ok, 0)::bigint AS ok,
               CASE
                 WHEN c.sent > 0 THEN 1.0 - COALESCE(d.ok, 0)::double precision / c.sent
                 ELSE NULL
               END AS loss_rate,
               CASE
                 WHEN EXTRACT(EPOCH FROM (c.stopped_at - c.started_at)) > 0
                 THEN COALESCE(d.ok, 0)::double precision
                      / EXTRACT(EPOCH FROM (c.stopped_at - c.started_at))
                 ELSE NULL
               END AS return_pps
        FROM just_tx_config c
        LEFT JOIN (
            SELECT config_id, COUNT(*) FILTER (WHERE got_rx) AS ok
            FROM just_tx_data
            GROUP BY config_id
        ) d ON d.config_id = c.id
        {where}
        ORDER BY c.fw_type, c.target_pps, c.started_at
        """,
        params,
    )


def load_just_tx_rtts(config_ids: list[int] | None = None) -> pd.DataFrame:
    """Successful-packet RTT samples (ms) from ``just_tx_data``."""
    empty = pd.DataFrame(columns=["config_id", "t_tx", "t_rx", "rtt_ms"])
    if config_ids is not None and len(config_ids) == 0:
        return empty
    if config_ids is None:
        return read_sql(
            """
            SELECT config_id, t_tx, t_rx,
                   EXTRACT(EPOCH FROM (t_rx - t_tx)) * 1000.0 AS rtt_ms
            FROM just_tx_data
            WHERE got_rx AND t_rx IS NOT NULL
            ORDER BY config_id, t_tx
            """
        )
    return read_sql(
        """
        SELECT config_id, t_tx, t_rx,
               EXTRACT(EPOCH FROM (t_rx - t_tx)) * 1000.0 AS rtt_ms
        FROM just_tx_data
        WHERE got_rx AND t_rx IS NOT NULL
          AND config_id = ANY(%(ids)s::bigint[])
        ORDER BY config_id, t_tx
        """,
        {"ids": config_ids},
    )


def load_just_tx_rtt_points(*, fw_type: str | None = None) -> pd.DataFrame:
    """Per-packet RTT: ``config_id``, ``fw_type``, ``target_pps``, ``rtt_ms``."""
    clauses = ["d.got_rx", "d.t_rx IS NOT NULL"]
    params: dict = {}
    if fw_type is not None:
        clauses.append("c.fw_type = %(fw_type)s")
        params["fw_type"] = fw_type
    where = " AND ".join(clauses)
    return read_sql(
        f"""
        SELECT c.id AS config_id, c.fw_type, c.target_pps,
               EXTRACT(EPOCH FROM (d.t_rx - d.t_tx)) * 1000.0 AS rtt_ms
        FROM just_tx_data d
        JOIN just_tx_config c ON c.id = d.config_id
        WHERE {where}
        ORDER BY c.fw_type, c.target_pps, d.t_tx
        """,
        params if params else None,
    )


def load_just_tx_rtt_runs(
    *,
    run_kind: str | None = "pilot",
    status: str = "complete",
    source_note: str | None = "xpost-test just-tx-rtt",
    phase: str | None = None,
) -> pd.DataFrame:
    clauses = ["status = %(status)s"]
    params: dict = {"status": status}
    if run_kind is not None:
        clauses.append("run_kind = %(run_kind)s")
        params["run_kind"] = run_kind
    if source_note is not None:
        clauses.append("source_note = %(source_note)s")
        params["source_note"] = source_note
    if phase is not None:
        clauses.append("phase = %(phase)s")
        params["phase"] = phase
    where = " AND ".join(clauses)
    return read_sql(
        f"""
        SELECT run_id::text AS run_id, pair_id, run_kind, variant, phase,
               target_pps, calibrated_loss, window_secs,
               sent, ok, loss_rate, return_pps, device_rxcnt_delta,
               median_rtt_ms, p95_rtt_ms, mean_rtt_ms,
               started_at_utc, stopped_at_utc, status, source_note
        FROM just_tx_rtt_runs
        WHERE {where}
        ORDER BY variant, target_pps, pair_id, started_at_utc
        """,
        params,
    )


def load_just_tx_rtt_samples(run_ids: list[str] | None = None) -> pd.DataFrame:
    if run_ids is not None and len(run_ids) == 0:
        return pd.DataFrame(columns=["run_id", "seq", "t0", "ok", "rtt_ms"])
    if run_ids is None:
        return read_sql(
            """
            SELECT run_id::text AS run_id, seq, t0, ok, rtt_ms
            FROM just_tx_rtt_samples
            ORDER BY run_id, seq
            """
        )
    return read_sql(
        """
        SELECT run_id::text AS run_id, seq, t0, ok, rtt_ms
        FROM just_tx_rtt_samples
        WHERE run_id = ANY(%(ids)s::uuid[])
        ORDER BY run_id, seq
        """,
        {"ids": run_ids},
    )


def load_just_drop_config(*, fw_type: str | None = None) -> pd.DataFrame:
    """Counter-kind windows from ``just_drop_config``."""
    clauses: list[str] = []
    params: dict = {}
    if fw_type is not None:
        clauses.append("fw_type = %(fw_type)s")
        params["fw_type"] = fw_type
    where = f"WHERE {' AND '.join(clauses)}" if clauses else ""
    return read_sql(
        f"""
        SELECT id AS config_id, fw_type, target_pps, started_at, stopped_at, sent, counters,
               EXTRACT(EPOCH FROM (stopped_at - started_at))::double precision AS duration_s
        FROM just_drop_config
        {where}
        ORDER BY fw_type, started_at
        """,
        params if params else None,
    )


def load_just_drop_data(config_ids: list[int] | None = None) -> pd.DataFrame:
    """1 Hz ``drop`` samples for Counter windows."""
    empty = pd.DataFrame(columns=["config_id", "t", "counter_value", "time_ns"])
    if config_ids is not None and len(config_ids) == 0:
        return empty
    if config_ids is None:
        df = read_sql(
            """
            SELECT config_id, t, counter_value,
                   (EXTRACT(EPOCH FROM t) * 1e9)::bigint AS time_ns
            FROM just_drop_data
            ORDER BY config_id, t
            """
        )
    else:
        df = read_sql(
            """
            SELECT config_id, t, counter_value,
                   (EXTRACT(EPOCH FROM t) * 1e9)::bigint AS time_ns
            FROM just_drop_data
            WHERE config_id = ANY(%(ids)s::bigint[])
            ORDER BY config_id, t
            """,
            {"ids": config_ids},
        )
    return df if len(df) else empty


def load_just_drop_run(*, kind: str | None = None, fw_type: str | None = None) -> pd.DataFrame:
    """Fixed-pps Aggregate / Gauge / Trace rounds from ``just_drop_run``."""
    clauses: list[str] = []
    params: dict = {}
    if kind is not None:
        clauses.append("kind = %(kind)s")
        params["kind"] = kind
    if fw_type is not None:
        clauses.append("fw_type = %(fw_type)s")
        params["fw_type"] = fw_type
    where = f"WHERE {' AND '.join(clauses)}" if clauses else ""
    return read_sql(
        f"""
        SELECT id AS run_id, fw_type, drop_site, kind, target_pps, window_s,
               started_at, stopped_at, sent, counters
        FROM just_drop_run
        {where}
        ORDER BY kind, fw_type, started_at
        """,
        params if params else None,
    )


def load_just_drop_agg(run_ids: list[int] | None = None) -> pd.DataFrame:
    empty = pd.DataFrame(
        columns=[
            "run_id",
            "name",
            "count",
            "total_ticks",
            "min_ticks",
            "max_ticks",
            "lo",
            "width",
            "buckets",
            "mean_ticks",
        ]
    )
    if run_ids is not None and len(run_ids) == 0:
        return empty
    if run_ids is None:
        df = read_sql(
            """
            SELECT run_id, name, count, total_ticks, min_ticks, max_ticks, lo, width, buckets,
                   CASE WHEN count > 0
                        THEN total_ticks::double precision / count
                        ELSE NULL END AS mean_ticks
            FROM just_drop_agg
            ORDER BY run_id, name
            """
        )
    else:
        df = read_sql(
            """
            SELECT run_id, name, count, total_ticks, min_ticks, max_ticks, lo, width, buckets,
                   CASE WHEN count > 0
                        THEN total_ticks::double precision / count
                        ELSE NULL END AS mean_ticks
            FROM just_drop_agg
            WHERE run_id = ANY(%(ids)s::bigint[])
            ORDER BY run_id, name
            """,
            {"ids": run_ids},
        )
    return df if len(df) else empty


def load_just_drop_gauge(run_ids: list[int] | None = None) -> pd.DataFrame:
    empty = pd.DataFrame(columns=["run_id", "name", "current", "peak"])
    if run_ids is not None and len(run_ids) == 0:
        return empty
    if run_ids is None:
        df = read_sql(
            """
            SELECT run_id, name, current, peak
            FROM just_drop_gauge
            ORDER BY run_id, name
            """
        )
    else:
        df = read_sql(
            """
            SELECT run_id, name, current, peak
            FROM just_drop_gauge
            WHERE run_id = ANY(%(ids)s::bigint[])
            ORDER BY run_id, name
            """,
            {"ids": run_ids},
        )
    return df if len(df) else empty


def load_just_drop_trace_summary(run_ids: list[int] | None = None) -> pd.DataFrame:
    empty = pd.DataFrame(columns=["run_id", "len", "dropped", "kind_hist"])
    if run_ids is not None and len(run_ids) == 0:
        return empty
    if run_ids is None:
        df = read_sql(
            """
            SELECT run_id, len, dropped, kind_hist
            FROM just_drop_trace_summary
            ORDER BY run_id
            """
        )
    else:
        df = read_sql(
            """
            SELECT run_id, len, dropped, kind_hist
            FROM just_drop_trace_summary
            WHERE run_id = ANY(%(ids)s::bigint[])
            ORDER BY run_id
            """,
            {"ids": run_ids},
        )
    return df if len(df) else empty


def load_counter_samples(run_ids: list[str] | None = None) -> pd.DataFrame:
    if run_ids is not None and len(run_ids) == 0:
        return pd.DataFrame(columns=["run_id", "time_ns", "counter_value"])
    if run_ids is None:
        return read_sql(
            """
            SELECT run_id::text AS run_id, time_ns, counter_value
            FROM counter_samples
            ORDER BY run_id, time_ns
            """
        )
    return read_sql(
        """
        SELECT run_id::text AS run_id, time_ns, counter_value
        FROM counter_samples
        WHERE run_id = ANY(%(ids)s::uuid[])
        ORDER BY run_id, time_ns
        """,
        {"ids": run_ids},
    )


def load_sample_reads(run_ids: list[str] | None = None) -> pd.DataFrame:
    if run_ids is not None and len(run_ids) == 0:
        return pd.DataFrame(
            columns=[
                "run_id",
                "sample_id",
                "triad_id",
                "tau_s",
                "t_req_ns",
                "t_resp_ns",
                "counter_value",
                "valid",
            ]
        )
    if run_ids is None:
        return read_sql(
            """
            SELECT run_id::text AS run_id, sample_id, triad_id, tau_s,
                   t_req_ns, t_resp_ns, counter_value, valid
            FROM sample_reads
            ORDER BY run_id, sample_id
            """
        )
    return read_sql(
        """
        SELECT run_id::text AS run_id, sample_id, triad_id, tau_s,
               t_req_ns, t_resp_ns, counter_value, valid
        FROM sample_reads
        WHERE run_id = ANY(%(ids)s::uuid[])
        ORDER BY run_id, sample_id
        """,
        {"ids": run_ids},
    )
