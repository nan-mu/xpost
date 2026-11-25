"""Plot qualified medium attempts selected by the runner's campaign audit."""

import argparse
import json
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

STYLES = {
    "hal": ("#3b3b3b", "o"), "aot": ("#00689a", "s"),
    "jit": ("#a74400", "^"), "rbpf": ("#00715c", "D"),
}
RATES = (400, 1400, 4000)
WORKLOADS = ("noop", "prime", "log2", "copy", "fletcher32", "bsort", "strcmp")


def network_fields(event):
    result = event["result"]
    tx = result.get("tx") or {}
    return {
        "offered": result.get("offered_pps"),
        "legal": result.get("legal_delivery_ratio"),
        "hit": result.get("illegal_hit_ratio"),
        "loss": result.get("qualified_loss"),
        "median_ms": milliseconds(tx.get("median_ns")),
        "p95_ms": milliseconds(tx.get("p95_ns")),
        "ingress": result.get("tagged_ingress_pps"),
        "drop": result.get("completed_drop_pps"),
    }


def milliseconds(nanoseconds):
    if nanoseconds is None:
        return None
    return float(nanoseconds) / 1_000_000


def plot(experiment, events, data, out):
    modes = data["modes"]
    phase = data["stage"]
    full = data.get("volume") == "all"
    if experiment == "exec_bench":
        categories = WORKLOADS
        batch_label = "DUT batch mean (µs)" if full else "DUT time per batch (µs)"
        round_label = "DUT round mean (µs)" if full else "DUT time per round (µs)"
        panels = (("batch_us", batch_label), ("us_per_round", round_label))
    else:
        categories = data.get("rates", RATES)
        per_experiment = {
            "firewall": (("legal", "Legal delivery fraction"), ("hit", "Target-key hit fraction")),
            "just_tx": (("loss", "Qualified loss fraction"), ("median_ms", "Median RTT (ms)"),
                        ("p95_ms", "95th-percentile RTT (ms)")),
            "just_drop": (("ingress", "DUT ingress (packets/s)"), ("drop", "Normal DROP (packets/s)")),
        }
        panels = (("offered", "Actual offered rate (packets/s)"), *per_experiment[experiment])

    fig, axs = plt.subplots(len(panels), 1, figsize=(10.4 if full else 8.2, 2.05 * len(panels) + .9),
                            sharex=True, layout="constrained")
    counts = []
    mode_spacing = .36 if full else .13
    repeat_spacing = .065 if full else .045
    marker_size = 23 if full else 32
    for i, mode in enumerate(modes):
        color, marker = STYLES[mode]
        entries = [e for e in events if e["mode"] == mode]
        xoffset = (i - (len(modes) - 1) / 2) * mode_spacing
        per_condition = []
        for category in categories:
            observations = [e for e in entries if (
                e["input"]["program"] if experiment == "exec_bench" else e["condition"]["pps"]
            ) == category]
            per_condition.append(len(observations))
            for ax, (field, _) in zip(axs, panels):
                x = categories.index(category) + xoffset
                for repeat, event in enumerate(observations):
                    fields = event if experiment == "exec_bench" else network_fields(event)
                    if fields.get(field) is not None:
                        ax.scatter(x + (repeat - (len(observations) - 1) / 2) * repeat_spacing,
                                   fields[field], color=color, marker=marker, s=marker_size)
        counts.append(f"{mode.upper()}  " + " · ".join(str(value) for value in per_condition))
    for ax, (_, label) in zip(axs, panels):
        ax.set_ylabel(label)
        ax.set_ylim(bottom=0)
        ax.grid(axis="y", alpha=.22)
        ax.spines[["top", "right"]].set_visible(False)
    axs[-1].set_xticks(range(len(categories)), categories)
    axs[-1].set_xlabel("Workload" if experiment == "exec_bench" else "Target offered rate (packets/s)")
    handles = [axs[0].scatter([], [], color=STYLES[m][0], marker=STYLES[m][1], label=m.upper())
               for m in modes]
    axs[0].legend(handles=handles, frameon=False, ncols=len(modes), loc="best")
    subtitle = "Qualified attempts per condition in x-axis order"
    scope = ""
    if full and experiment == "exec_bench":
        scope = f" · {data['samples_per_run']} batches per run"
    elif full:
        scope = f" · {data['send_s']} s send, {data['observe_s']} s observation"
    fig.suptitle(f"{experiment.replace('_', ' ')} · {phase.upper()}{scope}\n{subtitle}\n" + "    |    ".join(counts),
                 fontsize=11, fontweight="bold")
    fig.savefig(out / f"{experiment}.png", dpi=170)
    plt.close(fig)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    data = json.loads(args.input.read_text())
    accepted = set(data["accepted_run_ids"].values())
    events = [e for e in data["attempts"] if e["run_id"] in accepted and e["variant"] == "metrics"]
    args.output.mkdir(parents=True, exist_ok=True)
    for name in sorted({e["experiment"] for e in events}):
        plot(name, [e for e in events if e["experiment"] == name], data, args.output)


if __name__ == "__main__":
    main()
