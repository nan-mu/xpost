"""Evidence for function-call workloads, separate from packet-rate metrics."""
from itertools import product

import pandas as pd

from .metrics import HELPER_HIST_NAMES, combine_perf, microsecond_mean, pair_intervals, ratio

PRINTK_HELPER_ID = 6

WORKLOADS = ("incr", "square", "bitswap", "fib", "buf", "memcpy", "fletcher32", "bsort")


def attach_trace_runtime(config: pd.DataFrame, trace: pd.DataFrame) -> pd.DataFrame:
    """Subtract only complete, non-overlapping printk spans inside one invocation."""
    records = []
    if not trace.empty and not set(trace.config_id).issubset(set(config.config_id)):
        raise ValueError("exec_bench: Trace references another condition")
    for cid in config.config_id:
        missing = dict(invoke_us=float("nan"), printk_us=float("nan"),
                       runtime_us=float("nan"), trace_status="missing")
        if trace.empty:
            records.append(missing)
            continue
        events = trace[trace.config_id == cid]
        if events.empty:
            records.append(missing)
            continue
        if events.seq.duplicated().any() or not events.sort_values("seq").t_us.is_monotonic_increasing:
            raise ValueError(f"exec_bench: invalid Trace order for {cid}")
        intervals, unmatched = pair_intervals(events)
        if unmatched:
            raise ValueError(f"exec_bench: unmatched Trace events for {cid}")
        xdp = intervals[intervals.kind == "xdp_begin"]
        hp = intervals[(intervals.kind == "helper_enter") & (intervals.arg0 == PRINTK_HELPER_ID)]
        if xdp.empty or hp.empty:
            records.append(missing)
            continue
        if len(xdp) != 1:
            raise ValueError(f"exec_bench: expected one invocation for {cid}")
        span = xdp.iloc[0]
        hp = hp.sort_values("start_seq")
        if (not hp.start_seq.gt(span.start_seq).all() or not hp.end_seq.lt(span.end_seq).all()
                or hp.start_us.lt(span.start_us).any() or hp.end_us.gt(span.end_us).any()
                or span.end_arg0 != 2):
            raise ValueError(f"exec_bench: printk outside successful invocation for {cid}")
        if len(hp) > 1 and (hp.start_seq.iloc[1:].to_numpy() <= hp.end_seq.iloc[:-1].to_numpy()).any():
            raise ValueError(f"exec_bench: overlapping printk spans for {cid}")
        inv = span.end_us - span.start_us
        pr = (hp.end_us - hp.start_us).sum()
        if pr > inv:
            raise ValueError(f"exec_bench: printk duration exceeds invocation for {cid}")
        records.append(dict(invoke_us=inv, printk_us=pr, runtime_us=inv-pr, trace_status="complete"))
    for name in ("invoke_us", "printk_us", "runtime_us", "trace_status"):
        config[name] = [row[name] for row in records]
    return config


def derive_exec(tables: dict, settings: dict) -> tuple[dict, dict]:
    exam_id = settings["exams"]["exec_bench"]
    exams = tables["exams"].set_index("id")
    exam = exams.loc[exam_id]
    if pd.isna(exam.stopped_at):
        raise ValueError("exec_bench: pinned exam was not completed")
    config = tables["exec_bench_config"].copy()
    required = {"id", "exam_id", "firmware", "workload", "input", "started_at", "stopped_at"}
    if not required.issubset(config.columns):
        raise ValueError("exec_bench: missing configuration columns/rows")
    keys = config.groupby(["firmware", "workload"]).size()
    expected = set(product(("ebpf", "hal"), WORKLOADS))
    if set(keys.index) != expected or keys.nunique() != 1 or int(keys.iloc[0]) not in (1, 5):
        raise ValueError("exec_bench: each arm and workload must occur once or five times")
    if config.id.duplicated().any() or not config.exam_id.eq(exam_id).all():
        raise ValueError("exec_bench: duplicate IDs or another exam")
    raw = tables["exec_bench_data"]
    if not {"config_id", "rep", "ok", "output"}.issubset(raw.columns):
        raise ValueError("exec_bench: missing result columns/rows")
    if set(raw.config_id) != set(config.id) or raw.groupby("config_id").size().ne(1).any():
        raise ValueError("exec_bench: expected one result row per condition")
    config = config.merge(raw[["config_id", "rep", "ok", "output"]], left_on="id",
                          right_on="config_id", validate="one_to_one")
    reps = config.groupby(["firmware", "workload"]).rep.apply(lambda s: sorted(int(v) for v in s))
    if not reps.map(lambda s: s == list(range(1, len(s) + 1))).all():
        raise ValueError("exec_bench: repetitions must be numbered from 1 without gaps")
    if not config.ok.eq(True).all():
        raise ValueError("exec_bench: collector reported an output-check failure")
    for field in ("input", "output"):
        for value in config[field]:
            if not isinstance(value, str) or len(value) != 64 or len(bytes.fromhex(value)) != 32:
                raise ValueError(f"exec_bench: {field} must contain 32 hex-encoded bytes")
        if config.groupby("workload")[field].nunique().ne(1).any():
            raise ValueError(f"exec_bench: paired {field} values differ")
    start = pd.to_datetime(config.started_at, utc=True, format="mixed")
    stop = pd.to_datetime(config.stopped_at, utc=True, format="mixed")
    config["host_command_ms"] = (stop - start).dt.total_seconds() * 1000
    if config.host_command_ms.isna().any() or config.host_command_ms.lt(0).any():
        raise ValueError("exec_bench: invalid host command interval")
    result = {}
    for kind in ("agg", "counter", "gauge"):
        perf = combine_perf(tables, "exec_bench", kind)
        result[f"exec_bench_{kind}"] = perf.merge(
            config, on="config_id", suffixes=("_metric", ""), validate="many_to_one")
    agg = result["exec_bench_agg"]
    agg["mean_us"] = microsecond_mean(agg)
    for name in ("execution", "helper_execution"):
        metric = agg[agg.name == name].set_index("config_id")
        config[f"{name}_samples"] = config.config_id.map(metric["count"])
        config[f"{name}_mean_us"] = config.config_id.map(metric.mean_us)
    per = agg[agg.name.isin(HELPER_HIST_NAMES)]
    if not per.empty:
        summed = per.groupby("config_id")[["count", "total_ticks"]].sum()
        summed = summed[summed["count"] > 0]
        summed["mean_us"] = ratio(summed.total_ticks, summed["count"])
        missing = config["helper_execution_samples"].fillna(0).eq(0)
        hit = missing & config.config_id.isin(summed.index)
        config.loc[hit, "helper_execution_samples"] = config.loc[hit, "config_id"].map(summed["count"])
        config.loc[hit, "helper_execution_mean_us"] = config.loc[hit, "config_id"].map(summed.mean_us)
    config = attach_trace_runtime(config, tables["exec_bench_trace"])
    if settings.get("exec_bench", {}).get("require_trace", False) and config.runtime_us.isna().any():
        raise ValueError("exec_bench: pinned report requires complete invocation and printk Trace for all conditions")
    result["exec_bench"] = config
    audit = {
        "experiment": "exec_bench", "exam_id": exam_id,
        "started_at": exam.started_at, "stopped_at": exam.stopped_at,
        "conditions": len(config), "collector_passes": int(config.ok.sum()),
        "paired_inputs_and_outputs_equal": True,
        "execution_zero_sample_conditions": int(config.execution_samples.eq(0).sum()),
        "execution_missing_conditions": int(config.execution_samples.isna().sum()),
        "runtime_us_missing_conditions": int(config.runtime_us.isna().sum()),
        "trace_rows": len(tables["exec_bench_trace"]),
        "timing_scope": "runtime_us subtracts nested printk spans from Trace invocation; HAL includes output-map insertion, XDP excludes it",
        "build_scope": "libbpf-mcu compiles BPF and RV32IMC independently from C; the loaded image comes from the RV32IMC build",
    }
    return result, audit
