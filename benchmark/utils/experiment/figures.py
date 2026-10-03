"""Figure 1 (three bar panels) and Figure 2 (2×3 noise diagnostics)."""

from __future__ import annotations

from typing import Mapping, Sequence

import matplotlib.pyplot as plt
import numpy as np
from matplotlib.axes import Axes
from matplotlib.figure import Figure

# Okabe–Ito, tuned for a white paper background.
_PPS = "#0072B2"
_ADEV = "#D55E00"
_CV = "#009E73"
_INK = "#222222"
_MUTED = "#6E6E6E"
_FILL = "#D6E3F0"

_RC = {
    "font.family": "serif",
    "font.serif": ["STIXGeneral", "Times New Roman", "DejaVu Serif"],
    "mathtext.fontset": "stix",
    "axes.unicode_minus": False,
    "axes.linewidth": 0.7,
    "axes.edgecolor": _INK,
    "axes.labelcolor": _INK,
    "xtick.color": _INK,
    "ytick.color": _INK,
    "text.color": _INK,
    "xtick.direction": "in",
    "ytick.direction": "in",
    "xtick.major.size": 3.2,
    "ytick.major.size": 3.2,
    "xtick.major.width": 0.7,
    "ytick.major.width": 0.7,
    "xtick.minor.size": 1.8,
    "ytick.minor.size": 1.8,
    "legend.frameon": False,
    "pdf.fonttype": 42,
    "ps.fonttype": 42,
    "savefig.dpi": 200,
    "figure.facecolor": "white",
    "axes.facecolor": "white",
    "font.size": 9,
    "axes.labelsize": 9,
    "xtick.labelsize": 8,
    "ytick.labelsize": 8,
    "axes.titlesize": 10,
}


def _finite(x: float | None) -> bool:
    return x is not None and bool(np.isfinite(x))


def _pad_ylim(ax: Axes, values: Sequence[float], *, floor: float = 0.0) -> None:
    xs = [float(v) for v in values if np.isfinite(v)]
    if not xs:
        ax.set_ylim(floor, 1.0)
        return
    hi = max(xs)
    ax.set_ylim(floor, hi * 1.18 if hi > 0 else 1.0)


def figure1_summary(
    *,
    hal_mean_pps: float,
    xdp_mean_pps: float,
    hal_pps_points: Sequence[float] | None = None,
    xdp_pps_points: Sequence[float] | None = None,
    hal_pps_ci: tuple[float, float] | None = None,
    xdp_pps_ci: tuple[float, float] | None = None,
    hal_adev_1s: float | None = None,
    xdp_adev_1s: float | None = None,
    hal_adev_points: Sequence[float] | None = None,
    xdp_adev_points: Sequence[float] | None = None,
    hal_adev_ci: tuple[float, float] | None = None,
    xdp_adev_ci: tuple[float, float] | None = None,
    hal_cv: float | None = None,
    xdp_cv: float | None = None,
    hal_cv_ci: tuple[float, float] | None = None,
    xdp_cv_ci: tuple[float, float] | None = None,
    title: str = "HAL vs XDP",
) -> tuple[Figure, Axes]:
    """One shared axes host; categories HAL/XDP only; three Y scales.

    Returns (fig, host_ax). Twin axes are attached to host; do not create a 1×3 grid.
    Fractions (ADEV, CV) are drawn in percent. CI tuples are ``(lo, hi)`` in the
    same unit as the point (fraction, not percent).
    """
    with plt.rc_context(_RC):
        fig, host = plt.subplots(figsize=(6.8, 4.15))
        ax_adev = host.twinx()
        ax_cv = host.twinx()
        ax_adev.spines["left"].set_position(("axes", 0.5))
        ax_adev.spines["left"].set_visible(True)
        ax_adev.yaxis.set_label_position("left")
        ax_adev.yaxis.set_ticks_position("left")
        ax_cv.spines["right"].set_position(("axes", 1.0))

        for ax in (host, ax_adev, ax_cv):
            ax.spines["top"].set_visible(False)
        host.spines["right"].set_visible(False)
        ax_adev.spines["right"].set_visible(False)
        ax_cv.spines["left"].set_visible(False)
        ax_adev.spines["left"].set_linewidth(0.6)
        ax_adev.spines["left"].set_color(_ADEV)
        host.spines["left"].set_color(_PPS)
        ax_cv.spines["right"].set_color(_CV)

        cats = {"hal": 0.0, "xdp": 1.0}
        host.set_xlim(-0.55, 1.55)
        host.set_xticks([0.0, 1.0])
        host.set_xticklabels(["HAL", "XDP"])
        host.tick_params(axis="x", length=0, pad=6)

        def _err(point: float, ci: tuple[float, float] | None, scale: float = 1.0):
            if ci is None or not (_finite(ci[0]) and _finite(ci[1]) and _finite(point)):
                return None
            lo, hi = float(ci[0]), float(ci[1])
            # n is small: a CV interval of [0, ≈point] draws as a stem down to the axis.
            if lo <= 0.0 and hi <= point * 1.25:
                return None
            lo_s, hi_s = scale * lo, scale * hi
            y = scale * point
            return np.array([[max(0.0, y - lo_s)], [max(0.0, hi_s - y)]])

        # Mean PPS — blue circles, slightly left of the category.
        pps_vals: list[float] = []
        for name, mean, pts, ci, dx in (
            ("hal", hal_mean_pps, hal_pps_points, hal_pps_ci, -0.16),
            ("xdp", xdp_mean_pps, xdp_pps_points, xdp_pps_ci, -0.16),
        ):
            x = cats[name] + dx
            if pts:
                host.scatter(
                    [x] * len(pts),
                    list(pts),
                    s=16,
                    c=_PPS,
                    alpha=0.35,
                    linewidths=0,
                    zorder=2,
                )
                pps_vals.extend(float(p) for p in pts)
            if _finite(mean):
                pps_vals.append(float(mean))
                if ci is not None:
                    pps_vals.extend(float(v) for v in ci if _finite(v))
                host.errorbar(
                    [x],
                    [mean],
                    yerr=_err(float(mean), ci),
                    fmt="o",
                    color=_PPS,
                    markersize=7,
                    markeredgecolor="white",
                    markeredgewidth=0.6,
                    elinewidth=0.9,
                    capsize=3,
                    capthick=0.8,
                    zorder=3,
                    label="Mean PPS" if name == "hal" else None,
                )
        host.set_ylabel("Mean PPS (packets/s)", color=_PPS)
        host.tick_params(axis="y", colors=_PPS)
        host.yaxis.set_major_formatter(plt.FuncFormatter(lambda v, _: f"{v:,.0f}"))
        _pad_ylim(host, pps_vals)

        # Relative ADEV at 1 s — orange diamonds, on the category center.
        adev_vals: list[float] = []
        adev_missing = False
        for name, adev, pts, ci in (
            ("hal", hal_adev_1s, hal_adev_points, hal_adev_ci),
            ("xdp", xdp_adev_1s, xdp_adev_points, xdp_adev_ci),
        ):
            x = cats[name]
            if not _finite(adev):
                adev_missing = True
                continue
            if pts:
                ys = [100.0 * float(p) for p in pts if _finite(p)]
                ax_adev.scatter(np.full(len(ys), x), ys, s=14, c=_ADEV, alpha=0.35, linewidths=0, zorder=2)
                adev_vals.extend(ys)
            y = 100.0 * float(adev)
            adev_vals.append(y)
            if ci is not None:
                adev_vals.extend(100.0 * float(v) for v in ci if _finite(v))
            ax_adev.errorbar(
                [x],
                [y],
                yerr=_err(float(adev), ci, 100.0),
                fmt="D",
                color=_ADEV,
                markersize=5.5,
                markeredgecolor="white",
                markeredgewidth=0.5,
                elinewidth=0.9,
                capsize=3,
                capthick=0.8,
                zorder=3,
                label="Rel. ADEV at 1 s" if name == "hal" else None,
            )
        ax_adev.set_ylabel("Relative ADEV at 1 s (%)", color=_ADEV)
        ax_adev.tick_params(axis="y", colors=_ADEV, length=2.4, width=0.6)
        ax_adev.yaxis.label.set_color(_ADEV)
        _pad_ylim(ax_adev, adev_vals)

        # Between-run CV — green squares, slightly right.
        cv_vals: list[float] = []
        for name, cv, ci, dx in (
            ("hal", hal_cv, hal_cv_ci, 0.16),
            ("xdp", xdp_cv, xdp_cv_ci, 0.16),
        ):
            if not _finite(cv):
                continue
            x = cats[name] + dx
            y = 100.0 * float(cv)
            cv_vals.append(y)
            if ci is not None:
                cv_vals.extend(100.0 * float(v) for v in ci if _finite(v) and float(v) >= 0)
            ax_cv.errorbar(
                [x],
                [y],
                yerr=_err(float(cv), ci, 100.0),
                fmt="s",
                color=_CV,
                markersize=5.5,
                markeredgecolor="white",
                markeredgewidth=0.5,
                elinewidth=0.9,
                capsize=3,
                capthick=0.8,
                zorder=3,
                label="Between-run CV" if name == "hal" else None,
            )
        ax_cv.set_ylabel("Between-run CV (%)", color=_CV)
        ax_cv.tick_params(axis="y", colors=_CV)
        _pad_ylim(ax_cv, cv_vals)

        if title:
            host.set_title(title, pad=8, color=_INK, fontsize=11)
        handles, labels = [], []
        for ax in (host, ax_adev, ax_cv):
            h, lab = ax.get_legend_handles_labels()
            handles.extend(h)
            labels.extend(lab)
        if handles:
            host.legend(
                handles,
                labels,
                loc="upper center",
                bbox_to_anchor=(0.5, -0.12),
                ncol=3,
                fontsize=8,
                handletextpad=0.4,
                columnspacing=1.1,
            )
        if adev_missing:
            host.text(
                0.5,
                0.98,
                "ADEV not estimated",
                transform=host.transAxes,
                ha="center",
                va="top",
                fontsize=7.5,
                color=_MUTED,
            )
        fig.subplots_adjust(left=0.14, right=0.86, top=0.88, bottom=0.22)
        fig._xpost_figure1_axes = (host, ax_adev, ax_cv)  # type: ignore[attr-defined]
        return fig, host


def figure1_is_single_panel(fig: Figure) -> bool:
    """Structural check: one host position; not a 1×3 GridSpec of independent panels."""
    axes = list(fig.axes)
    if len(axes) < 1:
        return False
    bboxes = [tuple(ax.get_position().bounds) for ax in axes]
    x0s = {round(b[0], 4) for b in bboxes}
    widths = {round(b[2], 4) for b in bboxes}
    if len(x0s) > 1 and len(widths) > 1:
        return False
    host = axes[0]
    ticks = list(host.get_xticks())
    labels = [t.get_text() for t in host.get_xticklabels()]
    if len(ticks) != 2:
        return False
    return labels == ["HAL", "XDP"] or set(labels) == {"HAL", "XDP"}


def _scale_curve(
    ax: Axes,
    series: Mapping[float, float] | None,
    ci: Mapping[float, tuple[float, float]] | None,
    *,
    ylabel: str,
    scale: float = 1.0,
    href: float | None = None,
) -> None:
    ax.set_xscale("log")
    ax.set_xlabel(r"$\tau$ (s)")
    ax.set_ylabel(ylabel)
    if not series:
        ax.text(0.5, 0.5, "no scale estimate", ha="center", va="center", transform=ax.transAxes, color=_MUTED, fontsize=8)
        return
    taus = np.asarray(sorted(series), dtype=float)
    vals = np.asarray([scale * float(series[t]) for t in taus], dtype=float)
    if ci:
        lo, hi = [], []
        for t in taus:
            pair = ci.get(float(t))
            if pair is None or not (_finite(pair[0]) and _finite(pair[1])):
                lo.append(np.nan)
                hi.append(np.nan)
            else:
                lo.append(scale * float(pair[0]))
                hi.append(scale * float(pair[1]))
        lo_a, hi_a = np.asarray(lo), np.asarray(hi)
        ok = np.isfinite(lo_a) & np.isfinite(hi_a)
        if np.any(ok):
            ax.fill_between(taus[ok], np.maximum(lo_a[ok], 0.0), hi_a[ok], color=_PPS, alpha=0.18, linewidth=0)
    ax.plot(taus, vals, "-o", color=_PPS, ms=4.5, lw=1.15, markeredgecolor="white", markeredgewidth=0.4)
    if href is not None:
        ax.axhline(href, color=_MUTED, lw=0.7, ls="--", zorder=1)
        ax.text(
            taus[-1],
            href,
            " Poisson",
            ha="right",
            va="bottom",
            fontsize=7,
            color=_MUTED,
        )
    ax.set_xticks(list(taus))
    ax.set_xticklabels([f"{t:g}" for t in taus])
    ax.tick_params(axis="x", which="minor", bottom=False)


def figure2_noise(
    *,
    hal_adev: Mapping[float, float] | None = None,
    xdp_adev: Mapping[float, float] | None = None,
    hal_adev_ci: Mapping[float, tuple[float, float]] | None = None,
    xdp_adev_ci: Mapping[float, tuple[float, float]] | None = None,
    hal_counts: Sequence[float] | None = None,
    xdp_counts: Sequence[float] | None = None,
    hal_rate_ts: Sequence[tuple[float, float]] | None = None,
    xdp_rate_ts: Sequence[tuple[float, float]] | None = None,
    hal_af: Mapping[float, float] | None = None,
    xdp_af: Mapping[float, float] | None = None,
    hal_af_ci: Mapping[float, tuple[float, float]] | None = None,
    xdp_af_ci: Mapping[float, tuple[float, float]] | None = None,
    count_xlabel: str = "count in ~1 s window",
    model_scores: Mapping[str, Mapping[str, float]] | None = None,
    title: str = "",
) -> Figure:
    """One figure: 2 rows (HAL, XDP) × 3 cols (ADEV or rates, 1 s counts, holdout or AF).

    Prefer a τ-curve of relative ADEV when provided; otherwise plot equal-interval
    rate time series. Column 3 draws holdout scores when given, else Allan factor.
    """
    with plt.rc_context(_RC):
        fig, axes = plt.subplots(2, 3, figsize=(7.6, 5.15), sharex=False)
        rows = [
            ("HAL", axes[0], hal_adev, hal_adev_ci, hal_counts, hal_rate_ts, hal_af, hal_af_ci, "hal"),
            ("XDP", axes[1], xdp_adev, xdp_adev_ci, xdp_counts, xdp_rate_ts, xdp_af, xdp_af_ci, "xdp"),
        ]
        col_titles = ("Relative ADEV", "1 s window", "Holdout score" if model_scores else "Allan factor")
        for name, row, adev, adev_ci, counts, rate_ts, af, af_ci, key in rows:
            ax0, ax1, ax2 = row
            for ax in row:
                ax.spines["top"].set_visible(False)
                ax.spines["right"].set_visible(False)
            if adev:
                _scale_curve(ax0, adev, adev_ci, ylabel=f"{name}\nrel. ADEV (%)", scale=100.0)
            elif rate_ts:
                ts = [p[0] for p in rate_ts]
                rs = [p[1] for p in rate_ts]
                ax0.plot(ts, rs, "-", color=_PPS, lw=0.9, alpha=0.9)
                ax0.set_xlabel("t (s)")
                ax0.set_ylabel(f"{name}\ninterval PPS")
                ax0.set_ylim(bottom=0)
            else:
                ax0.text(0.5, 0.5, "no rate / ADEV", ha="center", va="center", transform=ax0.transAxes, color=_MUTED, fontsize=8)
                ax0.set_ylabel(name)

            if counts is not None and len(counts) > 0:
                vals = np.asarray(list(counts), dtype=float)
                bins = min(18, max(6, int(np.sqrt(vals.size))))
                ax1.hist(vals, bins=bins, color=_FILL, edgecolor="white", linewidth=0.6)
                ax1.axvline(float(np.mean(vals)), color=_PPS, lw=0.9)
                ax1.set_xlabel(count_xlabel)
                ax1.set_ylabel("windows")
            else:
                ax1.text(0.5, 0.5, "no count samples", ha="center", va="center", transform=ax1.transAxes, color=_MUTED, fontsize=8)

            scores = None
            if model_scores:
                scores = model_scores.get(key) or model_scores.get(name)
            if scores:
                names = list(scores.keys())
                bar_vals = [scores[n] for n in names]
                ax2.bar(names, bar_vals, color=_PPS, width=0.72)
                ax2.tick_params(axis="x", rotation=30)
                ax2.set_ylabel("holdout loglik")
                ax2.axhline(0.0, color=_MUTED, lw=0.5)
            elif af:
                _scale_curve(ax2, af, af_ci, ylabel="Allan factor", href=1.0)
            else:
                ax2.text(
                    0.5,
                    0.5,
                    "model holdout\nnot identified",
                    ha="center",
                    va="center",
                    transform=ax2.transAxes,
                    color=_MUTED,
                    fontsize=8,
                )
                ax2.set_xticks([])
                ax2.set_yticks([])
                for spine in ax2.spines.values():
                    spine.set_visible(False)

        for ax, col in zip(axes[0], col_titles):
            ax.set_title(col, fontsize=10, pad=6)
        if title:
            fig.suptitle(title, fontsize=11, y=1.02)
        fig.tight_layout()
        return fig
