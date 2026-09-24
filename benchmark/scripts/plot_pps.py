#!/usr/bin/env python3
"""Plot just_drop vs just_drop_hal PPS from just-drop-pps CSV.

Deps: pip install matplotlib

Usage:
  python3 plot_pps.py --hal results/hal_pps.csv --xdp results/xdp_pps.csv --outdir results
"""

from __future__ import annotations

import argparse
import csv
import json
import math
from collections import defaultdict
from pathlib import Path


def load_csv(path: Path):
    rows = []
    with path.open() as f:
        for row in csv.DictReader(f):
            rows.append(
                {
                    "path": row["path"],
                    "round": int(row["round"]),
                    "t_s": float(row["t_s"]),
                    "drop": int(row["drop"]),
                    "net_rx": int(row["net_rx"]),
                    "interval_pps": float(row["interval_pps"]),
                    "overall_pps_so_far": float(row["overall_pps_so_far"]),
                }
            )
    return rows


def overall_by_round(rows):
    """Last sample per round → overall_pps_so_far (≈ overall_pps)."""
    last = {}
    for r in rows:
        key = (r["path"], r["round"])
        prev = last.get(key)
        if prev is None or r["t_s"] >= prev["t_s"]:
            last[key] = r
    out = defaultdict(list)
    for (path, _rnd), r in sorted(last.items()):
        out[path].append(r["overall_pps_so_far"])
    return out


def mean_std(xs):
    if not xs:
        return 0.0, 0.0
    m = sum(xs) / len(xs)
    if len(xs) == 1:
        return m, 0.0
    v = sum((x - m) ** 2 for x in xs) / (len(xs) - 1)
    return m, math.sqrt(v)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--hal", type=Path, required=True)
    ap.add_argument("--xdp", type=Path, required=True)
    ap.add_argument("--outdir", type=Path, required=True)
    args = ap.parse_args()

    try:
        import matplotlib

        matplotlib.use("Agg")
        import matplotlib.pyplot as plt
    except ImportError as e:
        raise SystemExit("matplotlib required: pip install matplotlib") from e

    args.outdir.mkdir(parents=True, exist_ok=True)
    hal = load_csv(args.hal)
    xdp = load_csv(args.xdp)
    all_rows = hal + xdp

    # —— timeseries: interval pps ——
    fig, ax = plt.subplots(figsize=(9, 4.5))
    for path, color in (("hal", "#1f77b4"), ("xdp", "#d62728")):
        rows = [r for r in all_rows if r["path"] == path and r["t_s"] > 0]
        # average interval pps across rounds at similar t (bucket 1s)
        buckets = defaultdict(list)
        for r in rows:
            buckets[int(r["t_s"])].append(r["interval_pps"])
        if not buckets:
            continue
        xs = sorted(buckets)
        ys = [sum(buckets[t]) / len(buckets[t]) for t in xs]
        ax.plot(xs, ys, label=path, color=color, linewidth=1.5)
    ax.set_xlabel("t (s)")
    ax.set_ylabel("interval pps")
    ax.set_title("DUT drop interval pps during UDP blast")
    ax.legend()
    ax.grid(True, alpha=0.3)
    fig.tight_layout()
    ts_path = args.outdir / "pps_timeseries.png"
    fig.savefig(ts_path, dpi=140)
    plt.close(fig)

    # —— bar: overall pps mean±std ——
    by = overall_by_round(all_rows)
    labels = []
    means = []
    stds = []
    colors = []
    for path, color in (("hal", "#1f77b4"), ("xdp", "#d62728")):
        xs = by.get(path, [])
        if not xs:
            continue
        m, s = mean_std(xs)
        labels.append(path)
        means.append(m)
        stds.append(s)
        colors.append(color)

    fig, ax = plt.subplots(figsize=(5.5, 4.5))
    xs = range(len(labels))
    ax.bar(xs, means, yerr=stds, color=colors, capsize=6, width=0.55)
    ax.set_xticks(list(xs))
    ax.set_xticklabels(labels)
    ax.set_ylabel("overall pps")
    ax.set_title("DUT drop overall pps (mean ± std)")
    ax.grid(True, axis="y", alpha=0.3)
    for i, (m, s) in enumerate(zip(means, stds)):
        ax.text(i, m + s, f"{m:.0f}", ha="center", va="bottom", fontsize=9)
    fig.tight_layout()
    bar_path = args.outdir / "pps_compare_bar.png"
    fig.savefig(bar_path, dpi=140)
    plt.close(fig)

    summary = {
        path: {
            "rounds": by.get(path, []),
            "mean": mean_std(by.get(path, []))[0],
            "std": mean_std(by.get(path, []))[1],
        }
        for path in ("hal", "xdp")
        if path in by
    }
    sum_path = args.outdir / "summary.json"
    sum_path.write_text(json.dumps(summary, indent=2) + "\n")
    print(f"wrote {ts_path}")
    print(f"wrote {bar_path}")
    print(f"wrote {sum_path}")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
