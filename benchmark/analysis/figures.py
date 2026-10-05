"""Question-led figures; rendering never selects database runs or changes data."""
from __future__ import annotations

import os
from pathlib import Path

from .data import ROOT

os.environ.setdefault("MPLCONFIGDIR", str(ROOT / "outputs" / ".mplconfig"))
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib import font_manager
from matplotlib.colors import ListedColormap, BoundaryNorm
from matplotlib.patches import Patch
import numpy as np
import pandas as pd

from .metrics import ARMS, LABELS, pair_intervals

BLUE, ORANGE, INK, GRAY = "#0072B2", "#A65000", "#172B3A", "#526673"
ARM_STYLE = {"xdp_cag": dict(color=BLUE, marker="o", ls="-"), "hal": dict(color=ORANGE, marker="s", ls="--")}


def style():
    candidates = [Path("/System/Library/Fonts/Supplemental/Arial Unicode.ttf")]
    for path in candidates:
        if path.is_file():
            font_manager.fontManager.addfont(str(path))
            font = font_manager.FontProperties(fname=path).get_name()
            break
    else:
        installed = {item.name for item in font_manager.fontManager.ttflist}
        font = next((f for f in ["Noto Sans CJK SC", "Microsoft YaHei", "SimHei"] if f in installed), None)
        if font is None:
            raise RuntimeError("Install a CJK font (e.g. Noto Sans CJK SC) before building Chinese figures.")
    return plt.rc_context({
        "font.family": font, "font.size": 11, "axes.titlesize": 13,
        "axes.labelsize": 11, "legend.fontsize": 10, "figure.titlesize": 17,
        "text.color": INK, "axes.labelcolor": INK, "axes.edgecolor": "#ABB5BC",
        "axes.spines.top": False, "axes.spines.right": False,
        "axes.unicode_minus": False, "axes.grid": True, "grid.alpha": .18,
        "lines.linewidth": 2, "lines.markersize": 5, "figure.facecolor": "white",
        "axes.facecolor": "white", "savefig.facecolor": "white",
        "pdf.fonttype": 42, "svg.fonttype": "path", "savefig.bbox": None,
    })


def arms(ax, data, y, x="target_pps", labels=None):
    for arm in ("hal", "xdp_cag"):
        sub = data[data.firmware == arm].sort_values(x)
        ax.plot(sub[x], sub[y], label=(labels or ARMS)[arm], **ARM_STYLE[arm])
    ax.set(xlabel="主机设定发包速率（包/秒）", xlim=(250, 4150))
    ax.set_xticks([400, 1000, 1600, 2400, 3200, 4000])
    ax.legend(frameon=False, fontsize=9)


def save(fig, directory: Path, stem: str, metadata: list):
    fig.canvas.draw()
    files = []
    for ext in ("png", "svg", "pdf"):
        path = directory / f"{stem}.{ext}"
        fig.savefig(path, dpi=300, bbox_inches=None)
        files.append({"path": path.name, "bytes": path.stat().st_size})
    metadata.append({"figure": stem, "width_in": float(fig.get_figwidth()), "height_in": float(fig.get_figheight()), "dpi": 300, "files": files})
    plt.close(fig)


def quality(data, settings):
    rows, labels = [], []
    for e in ("just_drop", "just_tx", "firewall"):
        config = data[e].query("window_s > 0")
        for arm in ("hal", "xdp_cag"):
            cases = [(None, None)] if e != "firewall" else [(n, t) for n in settings["blacklist_sizes"] for t in settings["traffic"]]
            for n, mode in cases:
                sub = config[config.firmware == arm]
                label = ("ARRAY" if e == "just_drop" else "TX" if e == "just_tx" else "HASH") + " · " + ("HAL" if arm == "hal" else "XDP")
                if n is not None:
                    sub = sub[(sub.blacklist_size == n) & (sub.traffic == mode)]
                    mode_zh = {"hit": "命中", "miss": "未命中", "mixed": "混发"}.get(mode, mode)
                    label += f" · {n} 条 · {mode_zh}"
                rows.append(sub.set_index("target_pps").map_samples.reindex(settings["rates"]).to_numpy())
                labels.append(label)
    values = np.array(rows, dtype=float)
    status = np.where(np.isnan(values), 0, np.where(values == 0, 1, np.where(values < 100, 2, 3)))
    cmap = ListedColormap(["#F7F7F7", "#DDE2E5", "#F8D9A0", "#D0E7F2"])
    fig, ax = plt.subplots(figsize=(13, 7.3), layout="constrained")
    ax.imshow(status, cmap=cmap, norm=BoundaryNorm([-.5,.5,1.5,2.5,3.5], 4), aspect="auto")
    for i in range(len(rows)):
        for j in range(len(settings["rates"])):
            n = values[i,j]
            label = "缺行" if np.isnan(n) else (str(int(n)) if n < 1000 else f"{n/1000:.1f}k")
            ax.text(j, i, label, ha="center", va="center", fontsize=8.5, color=INK)
    ax.set_xticks(range(len(settings["rates"])), settings["rates"])
    ax.set_yticks(range(len(labels)), labels, fontsize=9)
    ax.set(xlabel="设定发送强度（包/秒）；格内为 map_operation 的计时样本数，k = 千次")
    ax.grid(False)
    fig.suptitle("附图 A · map_operation 计时样本覆盖")
    ax.legend(handles=[Patch(fc=cmap(i), label=s) for i,s in enumerate(["指标行缺失", "0 次：无计时样本", "1–99 次：样本很少", "≥100 次：有样本，仍需检查口径"])],
              loc="upper center", bbox_to_anchor=(.5, -.10), ncol=2, fontsize=9, frameon=False)
    return fig


def load_delivery(data):
    fig, axes = plt.subplots(1, 3, figsize=(13, 4.3), layout="constrained")
    for ax, e in zip(axes, ("just_drop", "firewall", "just_tx")):
        sub = data[e].query("window_s > 0")
        for arm in ("hal", "xdp_cag"):
            s = sub[sub.firmware == arm]
            ax.scatter(s.target_pps, s.sent_pps, label=ARMS[arm], color=ARM_STYLE[arm]["color"], marker=ARM_STYLE[arm]["marker"], s=26, alpha=.8)
        ax.plot([0,4100],[0,4100], ":", color=GRAY, label="实际 = 设定")
        ax.set(title=LABELS[e], xlabel="设定发送强度（包/秒）", ylabel="实际发出 ÷ 30 秒（包/秒）", xlim=(0,4200), ylim=(0,4200))
        ax.legend(fontsize=8, frameon=False)
    fig.suptitle("附图 B · Offered load：设定值与主机实际发出值")
    return fig


def map_points(data, experiment):
    """Keep missing metric rows as gaps on the complete configuration grid."""
    config = data[experiment].query("window_s > 0")
    metrics = data[f"{experiment}_agg"].query("name == 'map_operation'")
    return config.merge(metrics[["config_id", "mean_us"]], left_on="id", right_on="config_id",
                        how="left", validate="one_to_one")


def array(data):
    sub = data["just_drop"].query("window_s > 0")
    a = map_points(data, "just_drop")
    fig, axes = plt.subplots(1, 2, figsize=(12, 4.7), layout="constrained")
    arms(axes[0], sub, "map_per_sent_pct", labels={
        "hal": "HAL：应用收到 UDP 后计数", "xdp_cag": "XDP：驱动收到包后计数"})
    axes[0].axhline(100, color=GRAY, ls=":", lw=1)
    axes[0].set(title="(a) 完成计数的比例", ylabel="数组计数总和 / 主机发送包数（%）", ylim=(0,110))
    arms(axes[1], a, "mean_us", labels={
        "hal": "HAL：取下标并将元素加一", "xdp_cag": "XDP：查找数组元素（不含加一）"})
    axes[1].set(title="(b) 单次操作耗时：两组测量的操作不同", ylabel="平均耗时（微秒）", ylim=(0, None))
    fig.suptitle("ARRAY · 每收到一个包，将对应数组元素加一")
    return fig


def array_flame(tables):
    """A time-ordered flame chart of observed nested intervals, not CPU samples."""
    events = tables["just_drop_trace"]
    intervals, unmatched = pair_intervals(events)
    chosen = None
    for row in intervals.query("kind == 'rx_begin' and arg0 == 43").sort_values("start_seq").itertuples():
        nested = intervals[(intervals.start_seq >= row.start_seq) & (intervals.end_seq <= row.end_seq)]
        if ((nested.kind == "xdp_begin") & (nested.end_arg0 == 1)).any():
            chosen = row
            break
    fig, ax = plt.subplots(figsize=(11, 3.3), layout="constrained")
    record = dict(experiment="just_drop", unmatched_events=unmatched, selected=chosen is not None)
    if chosen is None:
        ax.text(.5, .5, "缺少长度与动作相符的完整 RX 片段", transform=ax.transAxes, ha="center")
        return fig, record
    nested = intervals[(intervals.start_seq >= chosen.start_seq) & (intervals.end_seq <= chosen.end_seq)]
    duration = chosen.end_us - chosen.start_us
    levels = [("rx_begin", "驱动接收处理", GRAY), ("xdp_begin", "XDP 程序", BLUE), ("helper_enter", "数组查找函数", ORANGE)]
    for depth, (kind, label, color) in enumerate(levels):
        for row in nested[nested.kind == kind].itertuples():
            start = row.start_us - chosen.start_us
            width = row.end_us - row.start_us
            ax.barh(depth, width, left=start, height=.88, color=color, edgecolor="white")
            if width >= duration*.25:
                ax.text(start+width/2, depth, f"{label} · {width:g} µs", ha="center", va="center", color="white", fontsize=11)
            else:
                ax.text(start+width+2, depth, f"{label} · {width:g} µs", va="center", fontsize=11)
    ax.set(yticks=[], xlabel="从驱动开始处理此包算起（微秒）", xlim=(0, duration*1.04), ylim=(-.55, 2.65))
    ax.grid(axis="y", visible=False)
    fig.suptitle("ARRAY · 单包火焰时间线：数组查找位于 XDP 程序内部")
    record.update(start_seq=chosen.start_seq, end_seq=chosen.end_seq, duration_us=duration,
                  frame_bytes=43, events=len(events), intervals=nested.to_dict("records"))
    return fig, record


def hash_delivery(data):
    fw = data["firewall"].query("window_s > 0 and traffic == 'mixed'")
    if fw.empty:
        fw = data["firewall"].query("window_s > 0")
    size = int(fw.blacklist_size.mode().iloc[0]) if not fw.empty else 200
    sub = fw[fw.blacklist_size == size]
    fig, axes = plt.subplots(1, 2, figsize=(12, 4.4), layout="constrained")
    arms(axes[0], sub, "hit_record_pct", labels={
        "hal": "HAL：应用层拦截计数", "xdp_cag": "XDP：驱动入口拦截计数"})
    axes[0].axhline(100, color=GRAY, ls=":", lw=1)
    axes[0].set(title=f"(a) {size} 条黑名单：非法流命中计数",
                ylabel="命中计数 ÷ 非法流发出数（%）", ylim=(-2, 110))
    arms(axes[1], sub, "legal_delivery_pct", labels={
        "hal": "HAL：应用层收到合法 token", "xdp_cag": "XDP：放行后应用层收到合法 token"})
    axes[1].axhline(100, color=GRAY, ls=":", lw=1)
    axes[1].set(title="(b) 合法流：固定 1024 个唯一 u16",
                ylabel="匹配 token 数 ÷ 1024（%）", ylim=(-2, 110))
    fig.suptitle("HASH · 非法流拦截计数与合法流端到端交付")
    return fig


def hash_cost(data):
    fw = data["firewall"].query("window_s > 0 and traffic == 'mixed'")
    if fw.empty:
        fw = data["firewall"].query("window_s > 0")
    size = int(fw.blacklist_size.mode().iloc[0]) if not fw.empty else 200
    sub = fw[fw.blacklist_size == size]
    fig, axes = plt.subplots(1, 2, figsize=(12, 4.2), layout="constrained")
    labels = {"hal": "HAL：应用收包后计时", "xdp_cag": "XDP：驱动收包后计时"}
    arms(axes[0], sub, "rx_mean_us", labels=labels)
    axes[0].set(title="(a) 每次处理的平均耗时", ylabel="平均耗时（微秒）", ylim=(0, None))
    arms(axes[1], sub, "rx_timed_fraction_pct", labels=labels)
    axes[1].set(title="(b) 累计计时占发包窗口的比例", ylabel="累计计时 / 30 秒（%）", ylim=(0, None))
    fig.suptitle(f"HASH · {size} 条规则混发：计时从图例中的位置开始，不含此前处理")
    return fig


def tx_results(data):
    sub = data["just_tx"].query("window_s > 0")
    fig, axes = plt.subplots(2, 2, figsize=(12, 7.2), layout="constrained")
    panels = [("reply_pps", "回包吞吐率（reply throughput）", "匹配回复数 ÷ 30 秒（pps）"),
              ("loss_pct", "端到端丢包率（loss）", "未匹配回复数 ÷ 实际发出数（%）"),
              ("p50_ms", "RTT p50 · 成功回复的中位数", "往返时延（ms）"),
              ("p99_ms", "RTT p99 · 成功回复的尾延迟", "往返时延（ms）")]
    for ax, (metric, title, ylabel) in zip(axes.flat, panels):
        arms(ax, sub, metric)
        ax.set(title=title, ylabel=ylabel, ylim=(0, 100 if metric == "loss_pct" else None))
        ax.axvspan(1200, 1400, color=GRAY, alpha=.12, zorder=0)
    knee = sub.query("firmware == 'xdp_cag'").set_index("target_pps").p50_ms
    axes[1, 0].annotate(f"XDP 中位数\n{knee.loc[1200]:.2f} → {knee.loc[1400]:.2f} ms", xy=(1400, knee.loc[1400]),
                        xytext=(1900, 25), fontsize=9, arrowprops=dict(arrowstyle="->", color=GRAY))
    fig.suptitle("TX · 同时评价回包吞吐率、丢包率与 RTT（灰带：1200–1400 pps）")
    return fig


def tx_cdf(data):
    config = data["just_tx"].query("window_s > 0").set_index("id")
    raw = data["rtt_samples"].join(config[["firmware", "target_pps"]], on="config_id")
    fig, axes = plt.subplots(2, 2, figsize=(12, 7.0), layout="constrained")
    for ax, rate in zip(axes.flat, [800, 1000, 1200, 1400]):
        for arm in ("hal", "xdp_cag"):
            values = np.sort(raw[(raw.firmware == arm) & (raw.target_pps == rate)].rtt_ms.to_numpy())
            # All observed values contribute. No subsampling, fitting, or pooling across conditions.
            values, counts = np.unique(values, return_counts=True)
            style = ARM_STYLE[arm]
            ax.step(values, 100*np.cumsum(counts)/counts.sum(), where="post", color=style["color"], ls=style["ls"],
                    label=f"{ARMS[arm]} · n={int(counts.sum()):,}")
        ax.set(xscale="log", xlabel="RTT（ms，对数坐标）", ylabel="成功回复的累计比例（%）",
               title=f"设定负载 {rate} pps", xlim=(.1, 1000), ylim=(0, 101))
        for y in (50, 95, 99):
            ax.axhline(y, color=GRAY, ls=":", lw=.7, alpha=.65)
        ax.legend(fontsize=8.5, frameon=False, loc="upper left")
    fig.suptitle("TX · 经验 CDF：尾延迟先恶化，随后中位数右移（只含成功回复）")
    return fig


def tx_pressure(data):
    sub = data["just_tx"].query("window_s > 0 and firmware == 'xdp_cag'").sort_values("target_pps")
    fig, axes = plt.subplots(1, 2, figsize=(12, 4.2), layout="constrained")
    for metric, label, color, marker, ls in [("tx_queue_depth_peak", "待提交队列", BLUE, "o", "-"),
                                            ("tx_inflight_peak", "提交中及未释放", GRAY, "s", "--")]:
        axes[0].plot(sub.target_pps, sub[metric], color=color, marker=marker, ls=ls, label=label)
    axes[0].set(title="XDP TX 的积压峰值", ylabel="峰值（包）", ylim=(0, None))
    axes[0].legend(frameon=False)
    axes[1].plot(sub.target_pps, sub.tx_enq_fail_claim, color=BLUE, marker="o")
    axes[1].set(title="XDP TX 取得发送资源失败", ylabel="claim 失败事件数 / 测试点", ylim=(0, None))
    for ax in axes:
        ax.set_xlabel("设定发送强度（pps）")
    fig.suptitle("附图 C · TX 队列压力：诊断线索，不作为 RTT 退化的唯一归因")
    return fig


def render_all(data, tables, settings, directory):
    directory.mkdir(parents=True, exist_ok=True)
    metadata = []
    with style():
        for stem, maker in [("array_results", lambda: array(data)), ("hash_delivery", lambda: hash_delivery(data)),
                            ("hash_cost", lambda: hash_cost(data)), ("tx_results", lambda: tx_results(data)),
                            ("tx_cdf", lambda: tx_cdf(data)), ("quality", lambda: quality(data, settings)),
                            ("load", lambda: load_delivery(data)), ("tx_pressure", lambda: tx_pressure(data))]:
            save(maker(), directory, stem, metadata)
        fig, trace = array_flame(tables)
        save(fig, directory, "array_flame", metadata)
    return metadata, trace
