"""Export figures and evidence for report.md. Never rewrite the report prose."""
from __future__ import annotations

import hashlib
import json
from pathlib import Path
import platform

from .data import ROOT, EXPERIMENTS, load_snapshot
from .metrics import derive
from .figures import render_all
import matplotlib
import numpy as np
import pandas as pd


def build(snapshot: Path, output: Path | None = None) -> Path:
    manifest, tables = load_snapshot(snapshot)
    data, audit = derive(tables, manifest["settings"])
    if "exec_bench" in manifest["settings"]["exams"]:
        from .exec_bench import derive_exec
        exec_data, exec_audit = derive_exec(tables, manifest["settings"])
        data.update(exec_data)
        audit["exec_bench"] = exec_audit
        if exec_audit["runtime_us_missing_conditions"]:
            audit["warnings"].append("EXEC has missing Trace timing; do not interpret missing values as zero latency.")
        audit["warnings"].append("EXEC Trace boundaries differ: HAL includes result-map insertion; do not report algorithm speedup ratios.")
    output = output or ROOT / "outputs" / "report"
    output.mkdir(parents=True, exist_ok=True)
    figures, trace = render_all(data, tables, manifest["settings"], output / "figures")
    audit["array_trace"] = trace
    if trace["unmatched_events"] or not trace["selected"]:
        audit["warnings"].append("ARRAY Trace has missing pairs or no eligible RX segment.")
    for experiment in manifest["settings"]["exams"]:
        data[experiment].to_csv(output / f"{experiment}_points.csv", index=False)
        for kind in ("agg", "counter", "gauge"):
            data[f"{experiment}_{kind}"].to_csv(output / f"{experiment}_{kind}.csv", index=False)
        tables[f"{experiment}_trace"].to_csv(output / f"{experiment}_trace.csv", index=False)
    pairs = data["just_tx"].query("window_s > 0").pivot(index="target_pps", columns="firmware",
                                                        values=["reply_pps", "loss_pct", "p50_ms", "p95_ms", "p99_ms"])
    pairs.to_csv(output / "tx_comparison.csv")
    if "exec_bench" in data:
        from .exec_bench import WORKLOADS
        comparison = data["exec_bench"].pivot(index="workload", columns="firmware",
                                              values=["runtime_us", "invoke_us", "printk_us", "ok"])
        comparison.reindex(WORKLOADS).to_csv(output / "exec_bench_comparison.csv")
    paths = list((ROOT / "analysis").glob("*.py")) + [
        ROOT / "report.md", ROOT / "report.toml",
        ROOT.parent / "src/helper/mod.rs", ROOT.parent / "src/map/ops.rs",
        ROOT.parent / "src/ext/perf/aggregate.rs", ROOT.parent / "src/ext/perf/trace/esp_radio_trace.rs",
        ROOT.parent / "esp-radio/src/wifi/xdp/tx.rs", ROOT.parent / "esp-radio/src/wifi/mod.rs",
        ROOT.parent / "examples/hal_perf/src/lib.rs",
        *[ROOT.parent / f"examples/{e}_hal/src/bin/main.rs" for e in EXPERIMENTS],
        *[ROOT.parent / f"xpost-test/common/src/exam/{e}.rs" for e in EXPERIMENTS],
    ]
    if "exec_bench" in manifest["settings"]["exams"]:
        paths += [ROOT.parent / p for p in (
            "xpost-test/common/src/exam/exec_bench.rs", "xpost-test/config/exec_bench.toml",
            "xpost-test/sql/exec_bench.sql", "examples/exec_bench/src/bin/main.rs",
            "examples/exec_bench_hal/src/bin/main.rs", "examples/exec_bench_hal/src/lib.rs",
            "src/ext/exec_call/mod.rs", "src/xdp/ingress/mod.rs", "profiles.toml",
            "src/helper/inner.rs",
        )]
    sibling_files = {}
    if "exec_bench" in manifest["settings"]["exams"]:
        from .exec_bench import WORKLOADS
        sibling = ROOT.parent.parent / "libbpf-mcu"
        for name in [*[f"tests/xdp/{w}.c" for w in WORKLOADS],
                     "make/build.py", "make/ninja/esp32c3.ninja", "make/ninja/bpf.ninja"]:
            path = sibling / name
            sibling_files[f"../libbpf-mcu/{name}"] = (
                hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else None)
    provenance = dict(
        snapshot=str(snapshot), source_sha256=manifest["source_sha256"], settings=manifest["settings"],
        versions=dict(python=platform.python_version(), matplotlib=matplotlib.__version__,
                      pandas=pd.__version__, numpy=np.__version__),
        source_files={str(p.relative_to(ROOT.parent)): hashlib.sha256(p.read_bytes()).hexdigest() for p in paths},
        sibling_source_files=sibling_files,
        source_hash_scope="Current analysis-time sources, not a record of the firmware flashed during collection.",
        figures=figures, uncertainty="one run per condition; no across-run confidence intervals",
        narrative="report.md is maintained separately. Review its conclusions after changing exams.",
    )
    for name, value in (("audit", audit), ("provenance", provenance)):
        (output / f"{name}.json").write_text(json.dumps(value, ensure_ascii=False, indent=2))
    return output / "figures"
