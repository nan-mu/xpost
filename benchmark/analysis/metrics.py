"""Metric definitions and evidence checks, independent of rendering."""
from __future__ import annotations

from itertools import product
import numpy as np
import pandas as pd

from .data import EXPERIMENTS

LABELS = {"just_drop": "按来源计数（ARRAY）", "firewall": "查黑名单（HASH）", "just_tx": "原路返回（XDP_TX）"}
ARMS = {"hal": "HAL：应用层处理", "xdp_cag": "XDP：驱动入口处理"}
MAP_HELPER_NAMES = ("map_lookup", "map_update", "map_delete")
HELPER_HIST_NAMES = MAP_HELPER_NAMES + (
    "ktime_get_ns", "trace_printk", "csum_diff", "xdp_adjust_tail",
)
# These totals are rdcycle counts. They are not pipeline microseconds.
LOOKUP_CYCLE_NAMES = ("lookup_non_null", "lookup_null", "lookup_timer_empty")


def ratio(numerator, denominator):
    """Undefined denominators stay missing, never zero-filled."""
    return numerator / denominator.where(denominator > 0)


def microsecond_mean(frame):
    """Mean for microsecond spans. Lookup cycle rows stay missing."""
    mean = ratio(frame.total_ticks, frame["count"])
    if "name" in frame.columns:
        mean = mean.mask(frame.name.isin(LOOKUP_CYCLE_NAMES))
    return mean


def map_helper_frame(agg: pd.DataFrame) -> pd.DataFrame:
    """One map-timing row per config.

    Prefer the per-helper histograms when they have samples.
    Older batches only have the lumped `map_operation` row.
    """
    cols = ["config_id", "count", "total_ticks"]
    lumped = agg.loc[agg.name == "map_operation", cols].copy()
    per = agg[agg.name.isin(MAP_HELPER_NAMES)]
    if per.empty:
        out = lumped
    else:
        summed = per.groupby("config_id", as_index=False)[["count", "total_ticks"]].sum()
        out = summed.merge(lumped, on="config_id", how="outer", suffixes=("_per", "_lumped"))
        use_per = out["count_per"].fillna(0).gt(0)
        out["count"] = out["count_per"].where(use_per, out["count_lumped"])
        out["total_ticks"] = out["total_ticks_per"].where(use_per, out["total_ticks_lumped"])
        out = out[cols]
    out = out.copy()
    out["mean_us"] = ratio(out.total_ticks, out["count"])
    return out


def cell_keys(frame: pd.DataFrame, experiment: str) -> list[tuple]:
    cols = ["firmware", "target_pps"]
    if experiment == "firewall":
        cols += ["blacklist_size", "traffic"]
    return list(frame[cols].itertuples(index=False, name=None))


def _stimulus_rounds(actual: pd.DataFrame):
    if "round" not in actual.columns or actual["round"].isna().all():
        return [actual]
    return [part for _, part in actual.groupby("round", sort=True)]


def validate_grid(config: pd.DataFrame, experiment: str, settings: dict) -> dict:
    required = {"id", "exam_id", "firmware", "target_pps", "window_s", "sent"}
    if not required.issubset(config.columns):
        raise ValueError(f"{experiment}: missing configuration columns/rows")
    actual = config[config.window_s > 0]
    expected = set(product(("hal", "xdp_cag"), settings["rates"]))
    if experiment == "firewall":
        expected = set(product(("hal", "xdp_cag"), settings["rates"], settings["blacklist_sizes"], settings["traffic"]))
    rounds = _stimulus_rounds(actual)
    for part in rounds:
        keys = cell_keys(part, experiment)
        duplicates = len(keys) - len(set(keys))
        missing, unexpected = expected - set(keys), set(keys) - expected
        if missing or unexpected or duplicates:
            raise ValueError(f"{experiment}: grid mismatch: missing={sorted(missing)}, unexpected={sorted(unexpected)}, duplicates={duplicates}")
    if not actual.window_s.eq(settings["window_s"]).all():
        raise ValueError(f"{experiment}: stimulus durations differ from report.toml")
    if not config.exam_id.eq(settings["exams"][experiment]).all():
        raise ValueError(f"{experiment}: snapshot has another exam")
    trace = config[config.firmware == "xdp_t"]
    if len(trace) != 1 or not trace.window_s.eq(0).all() or not trace.sent.eq(1).all():
        raise ValueError(f"{experiment}: expected one one-packet Trace configuration")
    if len(config) != len(actual) + len(trace):
        raise ValueError(f"{experiment}: unrecognized configurations outside the stimulus grid")
    if config.id.duplicated().any() or (actual.sent <= 0).any():
        raise ValueError(f"{experiment}: duplicate IDs or no sent packets")
    return {"experiment": experiment, "exam_id": config.exam_id.iloc[0], "cag_cells": len(actual), "trace_cells": len(trace), "rounds": len(rounds)}


def combine_perf(tables: dict, experiment: str, kind: str) -> pd.DataFrame:
    frames = [tables[f"{experiment}_{arm}_{kind}"] for arm in ("hal", "xdp")]
    frame = pd.concat(frames, ignore_index=True)
    required = ["config_id", "name"] + {"agg": ["total_ticks", "count", "buckets"], "counter": ["value"], "gauge": ["peak"]}[kind]
    if frame.empty:
        return pd.DataFrame(columns=required)
    if not set(required).issubset(frame.columns):
        raise ValueError(f"{experiment}: malformed {kind} measurements")
    if frame.duplicated(["config_id", "name"]).any():
        raise ValueError(f"{experiment}: duplicate {kind} measurements")
    return frame


def derive(tables: dict, settings: dict) -> tuple[dict[str, pd.DataFrame], dict]:
    result, audit = {}, {"grids": [], "warnings": [], "units": "pipeline device ticks = microseconds; lookup_* totals are rdcycle counts; host RTT = milliseconds"}
    exams = tables["exams"].set_index("id")
    for e in EXPERIMENTS:
        config = tables[e + "_config"].copy()
        grid = validate_grid(config, e, settings)
        exam = exams.loc[settings["exams"][e]]
        if pd.isna(exam.stopped_at):
            raise ValueError(f"{e}: pinned exam was not completed")
        grid.update(started_at=exam.started_at, stopped_at=exam.stopped_at)
        audit["grids"].append(grid)
        config["experiment"] = e
        config["sent_pps"] = ratio(config.sent, config.window_s)
        config["send_achievement_pct"] = 100 * ratio(config.sent_pps, config.target_pps)
        # started/stopped include warmup/drain in some pipelines; use declared stimulus window.
        for kind in ("agg", "counter", "gauge"):
            perf = combine_perf(tables, e, kind)
            result[f"{e}_{kind}"] = perf.merge(config, left_on="config_id", right_on="id", suffixes=("_metric", ""), validate="many_to_one")
        agg = result[f"{e}_agg"]
        agg["mean_us"] = microsecond_mean(agg)
        counts = map_helper_frame(agg).set_index("config_id")["count"]
        config["map_samples"] = config.id.map(counts)
        for metric in ("rx_packets", "rx_delivered", "xdp_pass", "xdp_drop", "rx_queue_overflow", "tx_enq_fail_claim", "tx_enq_fail_ring", "tx_fail_bad_buf", "tx_completed"):
            p = result[f"{e}_counter"]
            values = p[p.name == metric].set_index("config_id").value
            config[metric] = config.id.map(values)
        gauge = result[f"{e}_gauge"]
        for metric in ("rx_queue_depth", "tx_queue_depth", "tx_inflight"):
            values = gauge[gauge.name == metric].set_index("config_id").peak
            config[metric + "_peak"] = config.id.map(values)
        rx = agg[agg.name == "rx_processing"].set_index("config_id")
        config["rx_mean_us"] = config.id.map(rx.mean_us)
        # This is timed wall duration in instrumented regions, not CPU utilization.
        rx_total = config.id.map(rx.total_ticks.where(rx["count"] > 0))
        config["rx_timed_fraction_pct"] = 100 * ratio(rx_total, config.window_s * 1e6)
        if e != "just_tx":
            raw = tables[e + "_data"]
            totals = raw.groupby("config_id").value.sum(min_count=1)
            row_counts = raw.groupby("config_id").size()
            config["map_total"] = config.id.map(totals)
            config["map_rows"] = config.id.map(row_counts)
            config["map_per_sent_pct"] = 100 * ratio(config.map_total, config.sent)
            config["counted_pps"] = ratio(config.map_total, config.window_s)
            if e == "firewall":
                # Mixed cells: `sent` is illegal traffic; blacklist dump counts policy hits.
                mixed = config.traffic.eq("mixed") & config.window_s.gt(0)
                hit_only = config.traffic.eq("hit") & config.window_s.gt(0)
                config["hit_record_pct"] = config.map_per_sent_pct.where(mixed | hit_only)
                config["unaccounted_hit_pct"] = 100 - config.hit_record_pct
                # Legal-flow delivery: matched unique u16 tokens / legal_sent (collector stores this as `loss`).
                legal_den = config["legal_sent"] if "legal_sent" in config.columns else pd.Series(np.nan, index=config.index)
                matched = config["matched"] if "matched" in config.columns else pd.Series(np.nan, index=config.index)
                config["legal_delivery_pct"] = (100 * ratio(matched, legal_den)).where(mixed)
                config["legal_drop_pct"] = (100 - config.legal_delivery_pct).where(mixed)
                # Legacy miss-only queue metric (absent under mixed traffic).
                config["pass_queue_loss_pct"] = (100 * ratio(config.rx_queue_overflow, config.xdp_pass)).where(
                    config.firmware.eq("xdp_cag") & config.traffic.eq("miss") & config.window_s.gt(0))
            expected = 256 if e == "just_drop" else config.blacklist_size
            bad = (config.window_s > 0) & config.map_rows.ne(expected)
            if bad.any():
                audit["warnings"].append(f"{e}: {int(bad.sum())} cells have incomplete map dumps")
        else:
            raw = tables[e + "_data"].copy()
            raw["rtt_ms"] = (pd.to_datetime(raw.t_rx, utc=True, format="mixed") - pd.to_datetime(raw.t_tx, utc=True, format="mixed")).dt.total_seconds() * 1000
            if (raw.rtt_ms < 0).any() or raw.rtt_ms.isna().any():
                raise ValueError("RTT has negative or missing samples")
            stats = raw.groupby("config_id").rtt_ms.agg(
                rtt_n="size", p50_ms="median", p95_ms=lambda x: x.quantile(.95),
                p99_ms=lambda x: x.quantile(.99), max_ms="max",
            )
            config = config.merge(stats, left_on="id", right_index=True, how="left", validate="one_to_one")
            if not config.rtt_n.fillna(0).eq(config.ok).all():
                raise ValueError("RTT row count disagrees with config.ok")
            config["success_pct"] = 100 * ratio(config.ok, config.sent)
            config["loss_pct"] = 100 - config.success_pct
            config["reply_pps"] = ratio(config.ok, config.window_s)
            result["rtt_samples"] = raw
        result[e] = config
        cag = config[config.window_s > 0]
        grid["zero_sample_cells"] = int(cag.map_samples.eq(0).sum())
        grid["missing_sample_cells"] = int(cag.map_samples.isna().sum())
        grid["small_sample_cells"] = int(cag.map_samples.between(1, 99).sum())
        for field in ("zero_sample_cells", "missing_sample_cells", "small_sample_cells"):
            if grid[field]:
                audit["warnings"].append(f"{e}: {field}={grid[field]} for map timing")
        grid["min_send_achievement_pct"] = float(cag.send_achievement_pct.min())
        # Histogram mass is a consistency check, never an exact percentile estimator.
        if not all(sum(row.buckets) == row.count for row in agg.itertuples()):
            audit["warnings"].append(f"{e}: aggregate histogram counts disagree with sample counts")
    return result, audit


def pair_intervals(events: pd.DataFrame) -> tuple[pd.DataFrame, int]:
    """Pair only observed events. Do not invent endpoints for truncated rings."""
    columns = ["kind", "arg0", "start_us", "end_us", "start_seq", "end_seq", "end_arg0"]
    if events.empty:
        return pd.DataFrame(columns=columns), 0
    if "config_id" in events and events.config_id.nunique() > 1:
        raise ValueError("Pair Trace events from one configuration at a time.")
    starts = {"span_begin": "span_end", "helper_enter": "helper_leave", "rx_begin": "rx_end", "xdp_begin": "xdp_end"}
    ends = {v: k for k, v in starts.items()}
    stacks, intervals, unmatched = {}, [], 0
    for row in events.sort_values("seq").itertuples():
        if row.kind in starts:
            key = (row.kind, row.arg0 if row.kind in ("span_begin", "helper_enter") else 0)
            stacks.setdefault(key, []).append(row)
        elif row.kind in ends:
            begin = ends[row.kind]
            key = (begin, row.arg0 if begin in ("span_begin", "helper_enter") else 0)
            if not stacks.get(key):
                unmatched += 1
                continue
            first = stacks[key].pop()
            if row.t_us < first.t_us:
                unmatched += 2
                continue
            intervals.append({"kind": begin, "arg0": first.arg0, "start_us": first.t_us,
                              "end_us": row.t_us, "start_seq": first.seq, "end_seq": row.seq,
                              "end_arg0": row.arg0})
    unmatched += sum(len(v) for v in stacks.values())
    return pd.DataFrame(intervals, columns=columns), unmatched
