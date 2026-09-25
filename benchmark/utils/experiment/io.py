"""Load experiment_* tables into DataFrames (live Postgres)."""

from __future__ import annotations

import pandas as pd

from utils.db import read_sql


def load_runs(*, run_kind: str | None = None, status: str = "complete") -> pd.DataFrame:
    clauses = ["status = %(status)s"]
    params: dict = {"status": status}
    if run_kind is not None:
        clauses.append("run_kind = %(run_kind)s")
        params["run_kind"] = run_kind
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
