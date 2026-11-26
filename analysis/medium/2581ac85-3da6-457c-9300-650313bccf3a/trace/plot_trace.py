"""Render the formal DUT event timelines from trace-events.json."""

import argparse
import json
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.lines import Line2D
from matplotlib.ticker import MaxNLocator, StrMethodFormatter

MODE_STYLES = {
    "hal": ("HAL", "#3b3b3b", "o"),
    "aot": ("AOT", "#00689a", "s"),
    "jit": ("JIT", "#a74400", "^"),
    "rbpf": ("rBPF", "#00715c", "D"),
}

EVENT_LABELS = {
    "driver_rx": "Driver\nRX",
    "driver_rx_discarded": "RX\ndiscarded",
    "driver_rx_end": "Driver\nRX end",
    "driver_rx_enqueued": "RX\nenqueued",
    "driver_normal_drop": "Normal\nDROP",
    "driver_tx_enqueued": "Driver TX\nenqueued",
    "socket_rx": "Socket RX",
    "socket_discarded": "Socket\ndiscarded",
    "socket_delivered": "Socket\ndelivered",
    "socket_send_begin": "Socket\nsend begin",
    "socket_send_queued": "Socket\nsend queued",
    "xdp_run_enter": "XDP\nentry",
    "xdp_action_drop": "XDP\nDROP",
    "xdp_action_pass": "XDP\nPASS",
    "xdp_action_tx": "XDP\nTX",
    "xdp_policy_returned": "Policy\nreturn",
}

KEY_EVENTS = {"driver_normal_drop", "driver_tx_enqueued"}


def read_path(run):
    trace = run["trace"]
    if run["status"] != "complete" or not trace["qualified"] or not trace["complete"]:
        raise ValueError(f"Unqualified Trace {run['run_id']}")
    events = [event for event in run["events"] if event["source"] == "dut"]
    if not events or len(events) != trace["written"]:
        raise ValueError(f"Incomplete DUT event record {run['run_id']}")
    first = events[0]
    origin = int(run["trace_arm_raw"])
    freeze = int(run["trace_freeze_raw"])
    hz = first["clock_hz"]
    previous_seq = -1
    previous_tick = origin
    elapsed = []
    for event in events:
        seq = event["event_sequence"]
        tick = int(event["timestamp_raw"])
        if (event["clock_domain"] != "dut" or event["clock_id"] != trace["clock_id"]
                or event["clock_boot_id"] != trace["boot_id"] or event["clock_hz"] != hz
                or event["dictionary_version"] != trace["dictionary_version"]
                or event["probe_seq"] != first["probe_seq"] or hz <= 0
                or seq <= previous_seq or not previous_tick <= tick <= freeze):
            raise ValueError(f"Invalid packet event ordering or clock in {run['run_id']}")
        elapsed.append((tick - origin) * 1_000_000 / hz)
        previous_seq, previous_tick = seq, tick
    signature = [(e["event_name"], e["kind"], e["semantics"], e["boundary"]) for e in events]
    return signature, elapsed


def draw_panel(ax, title, runs):
    paths = [(run, *read_path(run)) for run in runs]
    signature = paths[0][1]
    if any(path_signature != signature for _, path_signature, _ in paths):
        raise ValueError(f"{title} mixes different event boundaries")
    names = [item[0] for item in signature]
    xs = list(range(len(names)))
    values = [value for _, _, elapsed in paths for value in elapsed]
    pad = max(45, (max(values) - min(values)) * 0.23)

    for run, _, elapsed in paths:
        _, color, marker = MODE_STYLES[run["mode"]]
        ax.plot(xs, elapsed, color=color, marker=marker, linewidth=1.9, markersize=6.2)
        for i, (name, value) in enumerate(zip(names, elapsed)):
            if i in (0, len(names) - 1) or name in KEY_EVENTS:
                vertical = 8
                if len(paths) > 1 and value == min(path[2][i] for path in paths):
                    vertical = -13
                horizontal = -6 if i == 0 else 0
                ax.annotate(f"{value:,.2f}".rstrip("0").rstrip("."), (i, value),
                            xytext=(horizontal, vertical), textcoords="offset points",
                            ha="center", color=color, fontsize=10, fontweight="bold")

    ax.set_title(title, loc="left", fontsize=14, fontweight="bold", pad=8)
    ax.set_xticks(xs, [EVENT_LABELS[name] for name in names])
    ax.set_xlim(-0.45, len(xs) - 0.55)
    ax.set_ylim(min(values) - pad, max(values) + pad)
    ax.yaxis.set_major_locator(MaxNLocator(4, integer=True))
    ax.yaxis.set_major_formatter(StrMethodFormatter("{x:,.0f}"))
    ax.grid(axis="y", color="#e0e4e7", linewidth=0.6)
    ax.set_axisbelow(True)
    ax.tick_params(axis="both", length=3.5, pad=5)
    ax.spines[["top", "right"]].set_visible(False)


def render(output, filename, title, panels, note):
    nrows = len(panels) // 2
    with plt.rc_context({
        "font.family": "serif", "font.serif": ["Times New Roman", "Liberation Serif", "DejaVu Serif"],
        "font.size": 11, "text.color": "#202127", "axes.labelcolor": "#202127",
        "axes.edgecolor": "#2a2a2a", "xtick.color": "#202127", "ytick.color": "#202127",
    }):
        fig = plt.figure(figsize=(11.15, 6.85 if nrows == 2 else 4.0), layout="constrained")
        fig.get_layout_engine().set(w_pad=0.08, h_pad=0.08)
        widths = (1, 1) if nrows == 2 else (0.76, 1.24)
        grid = fig.add_gridspec(nrows + 1, 2, height_ratios=[1] * nrows + [0.23],
                               width_ratios=widths)
        fig.suptitle(title, fontsize=20, fontweight="bold")
        fig.supylabel("Time from DUT Trace arm (μs)", fontsize=13)
        modes = {run["mode"] for _, runs in panels for run in runs}
        for idx, (label, runs) in enumerate(panels):
            draw_panel(fig.add_subplot(grid[idx // 2, idx % 2]), label, runs)
        footer = fig.add_subplot(grid[-1, :])
        footer.axis("off")
        handles = [Line2D([], [], color=color, marker=marker, linewidth=1.7, label=label)
                   for mode, (label, color, marker) in MODE_STYLES.items() if mode in modes]
        footer.legend(handles=handles, loc="center right", frameon=False, ncols=len(handles),
                      bbox_to_anchor=(1.0, 0.67), handlelength=1.4, columnspacing=1.1)
        footer.text(0.0, 0.70, "One tagged packet per line. Y-axis ranges vary by panel.",
                    ha="left", va="center", fontsize=10, color="#404751")
        footer.text(0.0, 0.13, note, ha="left", va="center", fontsize=10, color="#404751")
        fig.savefig(output / filename, dpi=180, facecolor="white")
        plt.close(fig)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path)
    parser.add_argument("output_dir", type=Path)
    args = parser.parse_args()
    data = json.loads(args.input.read_text(encoding="utf-8"))
    runs = {(r["experiment"], r["mode"], r["condition"]): r for r in data["runs"]}
    args.output_dir.mkdir(parents=True, exist_ok=True)

    render(args.output_dir, "firewall-trace.png", "Firewall event timestamps", [
        ("HAL · Blacklist hit", [runs["firewall", "hal", "hit"]]),
        ("HAL · Blacklist miss", [runs["firewall", "hal", "miss"]]),
        ("AOT · Blacklist hit", [runs["firewall", "aot", "hit"]]),
        ("AOT and JIT · Blacklist miss", [runs["firewall", "aot", "miss"],
                                         runs["firewall", "jit", "miss"]]),
    ], "JIT hit failed during setup. The campaign ended before the rBPF Trace runs.")

    render(args.output_dir, "just-tx-trace.png", "Just TX event timestamps", [
        ("HAL · Socket path", [runs["just_tx", "hal", "tagged"]]),
        ("AOT · XDP driver path", [runs["just_tx", "aot", "tagged"]]),
    ], "The send-side records show queueing. JIT and rBPF Trace runs were pending at interruption.")

    render(args.output_dir, "just-drop-trace.png", "Just DROP event timestamps", [
        ("HAL · Socket path", [runs["just_drop", "hal", "tagged"]]),
        ("AOT · XDP driver path", [runs["just_drop", "aot", "tagged"]]),
    ], "JIT and rBPF Trace runs were pending at interruption.")


if __name__ == "__main__":
    main()
