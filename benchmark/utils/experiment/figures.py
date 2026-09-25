"""Figure 1 (single axes, three Y) and Figure 2 (2×3 noise diagnostics)."""

from __future__ import annotations

from typing import Mapping, Sequence

import matplotlib.pyplot as plt
import numpy as np
from matplotlib.axes import Axes
from matplotlib.figure import Figure


def figure1_summary(
    *,
    hal_mean_pps: float,
    xdp_mean_pps: float,
    hal_pps_points: Sequence[float] | None = None,
    xdp_pps_points: Sequence[float] | None = None,
    hal_adev_1s: float | None = None,
    xdp_adev_1s: float | None = None,
    hal_cv: float | None = None,
    xdp_cv: float | None = None,
    title: str = "HAL vs XDP — mean PPS / ADEV(1s) / CV",
) -> tuple[Figure, Axes]:
    """One shared axes host; categories HAL/XDP only; three Y scales.

    Returns (fig, host_ax). Twin axes are attached to host; do not create a 1×3 grid.
    """
    fig, host = plt.subplots(figsize=(8.5, 5.0))
    # Keep a single host; twins share the same position.
    ax_adev = host.twinx()
    ax_cv = host.twinx()
    # Park middle axis spine between the two categories (x≈0.5 in axes fraction).
    ax_adev.spines["left"].set_position(("axes", 0.5))
    ax_adev.spines["left"].set_visible(True)
    ax_adev.yaxis.set_label_position("left")
    ax_adev.yaxis.set_ticks_position("left")
    ax_cv.spines["right"].set_position(("axes", 1.0))

    cats = {"hal": 0.0, "xdp": 1.0}
    host.set_xlim(-0.5, 1.5)
    host.set_xticks([0.0, 1.0])
    host.set_xticklabels(["HAL", "XDP"])

    # Mean PPS — blue circles (slight left offset)
    for name, mean, pts, dx in (
        ("hal", hal_mean_pps, hal_pps_points, -0.08),
        ("xdp", xdp_mean_pps, xdp_pps_points, -0.08),
    ):
        x = cats[name] + dx
        if pts:
            host.scatter(
                [x] * len(pts),
                pts,
                s=18,
                c="tab:blue",
                alpha=0.35,
                zorder=2,
            )
        if np.isfinite(mean):
            host.scatter([x], [mean], s=70, c="tab:blue", marker="o", zorder=3, label="Mean PPS" if name == "hal" else None)

    host.set_ylabel("Mean PPS (packets/s)", color="tab:blue")
    host.tick_params(axis="y", colors="tab:blue")
    host.set_ylim(bottom=0)

    # Relative ADEV at 1 s — orange diamonds (center)
    for name, adev, dx in (
        ("hal", hal_adev_1s, 0.0),
        ("xdp", xdp_adev_1s, 0.0),
    ):
        x = cats[name] + dx
        if adev is None or not np.isfinite(adev):
            host.annotate(
                "ADEV n/a",
                xy=(x, 0.02),
                xycoords=("data", "axes fraction"),
                ha="center",
                fontsize=8,
                color="tab:orange",
            )
        else:
            # Convert to percent for the middle axis
            ax_adev.scatter(
                [x],
                [100.0 * adev],
                s=70,
                c="tab:orange",
                marker="D",
                zorder=3,
                label="Rel. ADEV @ 1 s" if name == "hal" else None,
            )
    ax_adev.set_ylabel("Relative ADEV at 1 s (%)", color="tab:orange")
    ax_adev.tick_params(axis="y", colors="tab:orange")
    ax_adev.set_ylim(bottom=0)

    # CV — green squares (slight right offset)
    for name, cv, dx in (
        ("hal", hal_cv, 0.08),
        ("xdp", xdp_cv, 0.08),
    ):
        x = cats[name] + dx
        if cv is None or not np.isfinite(cv):
            continue
        ax_cv.scatter(
            [x],
            [100.0 * cv],
            s=70,
            c="tab:green",
            marker="s",
            zorder=3,
            label="Between-run CV" if name == "hal" else None,
        )
    ax_cv.set_ylabel("Between-run CV (%)", color="tab:green")
    ax_cv.tick_params(axis="y", colors="tab:green")
    ax_cv.set_ylim(bottom=0)

    host.set_title(title)
    # Combined legend from host only (markers already labeled once)
    handles, labels = [], []
    for ax in (host, ax_adev, ax_cv):
        h, l = ax.get_legend_handles_labels()
        handles.extend(h)
        labels.extend(l)
    if handles:
        host.legend(handles, labels, loc="upper left", fontsize=8)

    fig._xpost_figure1_axes = (host, ax_adev, ax_cv)  # type: ignore[attr-defined]
    return fig, host


def figure1_is_single_panel(fig: Figure) -> bool:
    """Structural check: one host position; not a 1×3 GridSpec of independent panels."""
    axes = list(fig.axes)
    if len(axes) < 1:
        return False
    # All axes should share the same bounding box (twins), not three side-by-side panels.
    bboxes = [tuple(ax.get_position().bounds) for ax in axes]
    # x0 and width should match for twins; reject if three distinct x0 values.
    x0s = {round(b[0], 4) for b in bboxes}
    widths = {round(b[2], 4) for b in bboxes}
    if len(x0s) > 1 and len(widths) > 1:
        # distinct panels
        return False
    host = axes[0]
    ticks = list(host.get_xticks())
    labels = [t.get_text() for t in host.get_xticklabels()]
    if len(ticks) != 2:
        return False
    return labels == ["HAL", "XDP"] or set(labels) == {"HAL", "XDP"}


def figure2_noise(
    *,
    hal_adev: Mapping[float, float] | None = None,
    xdp_adev: Mapping[float, float] | None = None,
    hal_counts: Sequence[int] | None = None,
    xdp_counts: Sequence[int] | None = None,
    model_scores: Mapping[str, Mapping[str, float]] | None = None,
    title: str = "Noise-model diagnostics (HAL / XDP)",
) -> Figure:
    """2 rows (HAL, XDP) × 3 cols (ADEV vs τ, count hist, holdout scores)."""
    fig, axes = plt.subplots(2, 3, figsize=(11, 6.5), sharex=False)
    variants = [
        ("HAL", axes[0], hal_adev, hal_counts),
        ("XDP", axes[1], xdp_adev, xdp_counts),
    ]
    for name, row, adev, counts in variants:
        ax0, ax1, ax2 = row
        ax0.set_ylabel(name)
        if adev:
            taus = sorted(adev.keys())
            vals = [adev[t] for t in taus]
            ax0.set_xscale("log")
            ax0.plot(taus, vals, "o-", color="tab:blue")
            ax0.set_xlabel("τ (s)")
            ax0.set_ylabel("Rel. ADEV")
        else:
            ax0.text(0.5, 0.5, "insufficient triad data", ha="center", va="center", transform=ax0.transAxes)
            ax0.set_xlabel("τ (s)")

        if counts is not None and len(counts) > 0:
            ax1.hist(list(counts), bins=min(20, max(5, len(counts) // 3)), color="tab:gray", edgecolor="k")
            ax1.set_xlabel("count in 1 s window")
        else:
            ax1.text(0.5, 0.5, "no count samples", ha="center", va="center", transform=ax1.transAxes)

        if model_scores and name.lower() in {k.lower() for k in model_scores}:
            # model_scores keyed by variant then model name → holdout ll
            key = "hal" if name == "HAL" else "xdp"
            scores = model_scores.get(key) or model_scores.get(name)
            if scores:
                names = list(scores.keys())
                vals = [scores[n] for n in names]
                ax2.bar(names, vals, color="tab:purple")
                ax2.tick_params(axis="x", rotation=30)
                ax2.set_ylabel("holdout loglik")
            else:
                ax2.text(0.5, 0.5, "no model scores", ha="center", va="center", transform=ax2.transAxes)
        else:
            ax2.text(0.5, 0.5, "evidence insufficient", ha="center", va="center", transform=ax2.transAxes)

    fig.suptitle(title)
    fig.tight_layout()
    return fig
