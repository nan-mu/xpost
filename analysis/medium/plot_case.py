"""Plot one verified A-to-B Case run from its packet and Trace facts."""

import argparse
import json
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt


def packet_ns(packet):
    return packet["ts_sec"] * 10**9 + packet["ts_subsec"] * (10**9 // packet["timestamp_hz"])


def plot_case(data, output):
    run = data["run_id"]
    switch = data["switch"]
    packets = data["packets"]
    events = data["events"]
    if not switch["qualified"] or switch["switch_index"] != 1 or not packets or not events:
        raise ValueError("Case needs one qualified measured switch and both evidence streams")
    if any(packet["observed_uuid"] != run for packet in packets):
        raise ValueError("Capture contains packet facts from another run")
    by_index = {p["record_index"]: p for p in packets}
    last_a = by_index[switch["last_old_record_index"]]
    first_b = by_index[switch["first_new_record_index"]]
    if (last_a["version_marker"], first_b["version_marker"]) != (2, 1):
        raise ValueError("Case evidence does not show the A-to-B transition")
    first_b_ns = packet_ns(first_b)

    def packet_ms(packet):
        return (packet_ns(packet) - first_b_ns) / 10**6

    fig, ax = plt.subplots(figsize=(10, 4.6), layout="constrained")
    for version, label, color, marker in [(2, "A · version 2", "#22577a", "o"),
                                          (1, "B · version 1", "#a13318", "^")]:
        group = [p for p in packets if p["version_marker"] == version]
        ax.scatter([packet_ms(p) for p in group], [p["seq"] for p in group],
                   color=color, marker=marker, label=label, s=16)
    ax.axvline(0, linewidth=1, color="#505050", linestyle="--")
    ax.set_xlabel("TR3000 packet capture time relative to first B reply (ms)")
    ax.set_ylabel("Probe sequence")
    ax.set_title(f"Tagged Case replies · run {run}")
    ax.legend(frameon=False)
    fig.savefig(output / "case-packet-window.png", dpi=170)
    plt.close(fig)

    fig, axes = plt.subplots(3, 1, figsize=(10, 6.0), layout="constrained")
    t0 = min(int(e["timestamp_raw"]) for e in events)
    labels = [(1, "Control"), (2, "Quiesce"), (3, "Rebind"), (4, "Publish")]
    for i, (event_id, _) in enumerate(labels):
        ticks = [int(e["timestamp_raw"]) - t0 for e in events if e["event_id"] == event_id]
        axes[0].scatter(ticks, [i] * len(ticks), marker="|", s=80, color="#22577a")
    axes[0].set_yticks(range(len(labels)), [label for _, label in labels])
    axes[0].invert_yaxis()
    axes[0].set_xlabel("DUT Trace time relative to first control event (µs)")
    host_ms = (int(switch["host_reply_ns"]) - int(switch["host_command_begin_ns"])) / 10**6
    axes[1].plot([0, host_ms], [0, 0], color="#5c2a69", marker="|", markersize=12)
    axes[1].set_title("Command send to acknowledgement", loc="left")
    axes[1].set_xlabel("TR3000 monotonic time relative to command send (ms)")
    axes[2].plot([packet_ms(last_a), 0], [0, 0], color="#505050", marker="|", markersize=12)
    axes[2].set_title(f"Last A probe {last_a['seq']} to first B probe {first_b['seq']}", loc="left")
    axes[2].set_xlabel("TR3000 packet capture time relative to first B reply (ms)")
    for ax in axes[1:]:
        ax.set_yticks([])
    fig.suptitle(f"Case switch · run {run}\nEach panel uses its own clock and origin")
    fig.savefig(output / "case-switch-timeline.png", dpi=170)
    plt.close(fig)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    plot_case(json.loads(args.input.read_text()), args.output)


if __name__ == "__main__":
    main()
