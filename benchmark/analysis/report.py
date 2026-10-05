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
    output = output or ROOT / "outputs" / "report"
    output.mkdir(parents=True, exist_ok=True)
    figures, trace = render_all(data, tables, manifest["settings"], output / "figures")
    audit["array_trace"] = trace
    if trace["unmatched_events"] or not trace["selected"]:
        audit["warnings"].append("ARRAY Trace has missing pairs or no eligible RX segment.")
    for experiment in EXPERIMENTS:
        data[experiment].to_csv(output / f"{experiment}_points.csv", index=False)
        for kind in ("agg", "counter", "gauge"):
            data[f"{experiment}_{kind}"].to_csv(output / f"{experiment}_{kind}.csv", index=False)
        tables[f"{experiment}_trace"].to_csv(output / f"{experiment}_trace.csv", index=False)
    pairs = data["just_tx"].query("window_s > 0").pivot(index="target_pps", columns="firmware",
                                                        values=["reply_pps", "loss_pct", "p50_ms", "p95_ms", "p99_ms"])
    pairs.to_csv(output / "tx_comparison.csv")
    paths = list((ROOT / "analysis").glob("*.py")) + [
        ROOT / "report.md", ROOT / "report.toml",
        ROOT.parent / "src/helper/mod.rs", ROOT.parent / "src/map/ops.rs",
        ROOT.parent / "src/ext/perf/aggregate.rs", ROOT.parent / "src/ext/perf/trace/esp_radio_trace.rs",
        ROOT.parent / "esp-radio/src/wifi/xdp/tx.rs", ROOT.parent / "esp-radio/src/wifi/mod.rs",
        ROOT.parent / "examples/hal_perf/src/lib.rs",
        *[ROOT.parent / f"examples/{e}_hal/src/bin/main.rs" for e in EXPERIMENTS],
        *[ROOT.parent / f"xpost-test/common/src/exam/{e}.rs" for e in EXPERIMENTS],
    ]
    provenance = dict(
        snapshot=str(snapshot), source_sha256=manifest["source_sha256"], settings=manifest["settings"],
        versions=dict(python=platform.python_version(), matplotlib=matplotlib.__version__,
                      pandas=pd.__version__, numpy=np.__version__),
        source_files={str(p.relative_to(ROOT.parent)): hashlib.sha256(p.read_bytes()).hexdigest() for p in paths},
        figures=figures, uncertainty="one run per condition; no across-run confidence intervals",
        narrative="report.md is maintained separately. Review its conclusions after changing exams.",
    )
    for name, value in (("audit", audit), ("provenance", provenance)):
        (output / f"{name}.json").write_text(json.dumps(value, ensure_ascii=False, indent=2))
    return output / "figures"
