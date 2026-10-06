"""Evidence for function-call workloads, separate from packet-rate metrics."""
from itertools import product

import pandas as pd

from .metrics import combine_perf, pair_intervals, ratio

PRINTK_HELPER_ID = 6

WORKLOADS = ("incr", "square", "bitswap", "fib", "buf", "memcpy", "fletcher32", "bsort")


def attach_trace_runtime(config: pd.DataFrame, trace: pd.DataFrame) -> pd.DataFrame:
    """Workload time is invoke span minus printk helper id 6."""
    invoke, printk, runtime = [], [], []
    empty = trace is None or trace.empty
    for cid in config.config_id:
        if empty:
            invoke.append(pd.NA)
            printk.append(pd.NA)
            runtime.append(pd.NA)
            continue
        events = trace[trace.config_id == cid]
        if events.empty:
            invoke.append(pd.NA)
            printk.append(pd.NA)
            runtime.append(pd.NA)
            continue
        intervals, _ = pair_intervals(events)
        xdp = intervals[intervals.kind == "xdp_begin"] if not intervals.empty else intervals
        hp = intervals[(intervals.kind == "helper_enter") & (intervals.arg0 == PRINTK_HELPER_ID)] if not intervals.empty else intervals
        inv = (xdp.end_us.iloc[0] - xdp.start_us.iloc[0]) if len(xdp) else pd.NA
        pr = (hp.end_us.iloc[0] - hp.start_us.iloc[0]) if len(hp) else pd.NA
        invoke.append(inv)
        printk.append(pr)
        if pd.isna(inv) or pd.isna(pr):
            runtime.append(pd.NA)
        else:
            runtime.append(inv - pr)
    config["invoke_us"] = invoke
    config["printk_us"] = printk
    config["runtime_us"] = runtime
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
    keys = list(config[["firmware", "workload"]].itertuples(index=False, name=None))
    if len(keys) != 16 or set(keys) != set(product(("ebpf", "hal"), WORKLOADS)):
        raise ValueError("exec_bench: expected one condition per arm and workload")
    if config.id.duplicated().any() or not config.exam_id.eq(exam_id).all():
        raise ValueError("exec_bench: duplicate IDs or another exam")
    raw = tables["exec_bench_data"]
    if not {"config_id", "rep", "ok", "output"}.issubset(raw.columns):
        raise ValueError("exec_bench: missing result columns/rows")
    if len(raw) != 16 or set(raw.config_id) != set(config.id) or not raw.rep.eq(1).all():
        raise ValueError("exec_bench: expected one result per condition with rep=1")
    config = config.merge(raw[["config_id", "rep", "ok", "output"]], left_on="id",
                          right_on="config_id", validate="one_to_one")
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
    agg["mean_us"] = ratio(agg.total_ticks, agg["count"])
    for name in ("execution", "helper_execution"):
        metric = agg[agg.name == name].set_index("config_id")
        config[f"{name}_samples"] = config.config_id.map(metric["count"])
        config[f"{name}_mean_us"] = config.config_id.map(metric.mean_us)
    config = attach_trace_runtime(config, tables["exec_bench_trace"])
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
        "timing_scope": "runtime_us is Trace (xdp_end-xdp_begin) minus printk helper id 6",
        "build_scope": "current build compiles C directly to RV32IMC; ebpf is the database arm label",
    }
    return result, audit
