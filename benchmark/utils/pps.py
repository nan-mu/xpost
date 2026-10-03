"""Queries for the just_drop vs just_drop_hal PPS experiment (live DB)."""

from __future__ import annotations

import pandas as pd

from utils.db import read_sql


def latest_runs() -> pd.DataFrame:
    """One row per path: latest just_drop_pps_run."""
    return read_sql(
        """
        SELECT DISTINCT ON (path)
            id AS run_id, path, created_at, window_secs, sample_interval_ms, note
        FROM just_drop_pps_run
        ORDER BY path, created_at DESC, id DESC
        """
    )


def rounds_for_latest() -> pd.DataFrame:
    """Round summaries joined to the latest run per path."""
    return read_sql(
        """
        WITH latest AS (
            SELECT DISTINCT ON (path) id AS run_id, path
            FROM just_drop_pps_run
            ORDER BY path, created_at DESC, id DESC
        )
        SELECT
            l.path,
            r.round,
            r.overall_pps,
            r.interval_pps_mean,
            r.interval_pps_std,
            r.host_pps,
            r.drop_delta,
            r.net_rx_delta,
            r.elapsed_secs
        FROM latest l
        JOIN just_drop_pps_round r ON r.run_id = l.run_id
        ORDER BY l.path, r.round
        """
    )


def path_summary() -> pd.DataFrame:
    """Mean ± std of overall_pps / host_pps across rounds for latest runs."""
    return read_sql(
        """
        WITH latest AS (
            SELECT DISTINCT ON (path) id AS run_id, path
            FROM just_drop_pps_run
            ORDER BY path, created_at DESC, id DESC
        )
        SELECT
            l.path,
            count(*)::int AS rounds,
            avg(r.overall_pps) AS overall_pps_mean,
            stddev_samp(r.overall_pps) AS overall_pps_std,
            avg(r.host_pps) AS host_pps_mean,
            avg(r.drop_delta) AS drop_delta_mean
        FROM latest l
        JOIN just_drop_pps_round r ON r.run_id = l.run_id
        GROUP BY l.path
        ORDER BY l.path
        """
    )


def samples_for_latest() -> pd.DataFrame:
    """Time-series samples for latest run per path."""
    return read_sql(
        """
        WITH latest AS (
            SELECT DISTINCT ON (path) id AS run_id, path
            FROM just_drop_pps_run
            ORDER BY path, created_at DESC, id DESC
        )
        SELECT
            l.path,
            r.round,
            s.t_s,
            s.drop_count,
            s.net_rx,
            s.interval_pps,
            s.overall_pps_so_far
        FROM latest l
        JOIN just_drop_pps_round r ON r.run_id = l.run_id
        JOIN just_drop_pps_sample s ON s.round_id = r.id
        ORDER BY l.path, r.round, s.t_s
        """
    )
