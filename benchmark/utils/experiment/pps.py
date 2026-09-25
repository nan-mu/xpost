"""Mean PPS, interval rates, between-run CV from raw (time, counter) series."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Sequence

import numpy as np


@dataclass(frozen=True)
class RunPps:
    run_id: str
    variant: str
    pair_id: int
    mean_pps: float | None
    elapsed_s: float
    counter_delta: int | None
    valid: bool
    reason: str = ""


def mean_pps(
    time_ns: Sequence[int],
    counter: Sequence[int],
    *,
    run_id: str = "",
    variant: str = "",
    pair_id: int = 0,
) -> RunPps:
    """Endpoint PPS = (C_end - C_start) / (t_end - t_start). Decline → invalid."""
    if len(time_ns) < 2 or len(counter) < 2:
        return RunPps(run_id, variant, pair_id, None, 0.0, None, False, "too_few_samples")
    t0, t1 = int(time_ns[0]), int(time_ns[-1])
    c0, c1 = int(counter[0]), int(counter[-1])
    elapsed = (t1 - t0) / 1e9
    if elapsed <= 0:
        return RunPps(run_id, variant, pair_id, None, elapsed, None, False, "nonpositive_dt")
    if c1 < c0:
        return RunPps(
            run_id, variant, pair_id, None, elapsed, c1 - c0, False, "counter_decrease"
        )
    delta = c1 - c0
    return RunPps(run_id, variant, pair_id, delta / elapsed, elapsed, delta, True)


def interval_rates(
    time_ns: Sequence[int],
    counter: Sequence[int],
) -> tuple[np.ndarray, np.ndarray, list[str]]:
    """Adjacent-window rates. Declines marked; corresponding rates are nan."""
    n = min(len(time_ns), len(counter))
    if n < 2:
        return np.array([]), np.array([]), []
    dts = []
    rates = []
    flags: list[str] = []
    for i in range(1, n):
        dt = (int(time_ns[i]) - int(time_ns[i - 1])) / 1e9
        dc = int(counter[i]) - int(counter[i - 1])
        dts.append(dt)
        if dt <= 0:
            rates.append(np.nan)
            flags.append("nonpositive_dt")
        elif dc < 0:
            rates.append(np.nan)
            flags.append("counter_decrease")
        else:
            rates.append(dc / dt)
            flags.append("ok")
    return np.asarray(dts, dtype=float), np.asarray(rates, dtype=float), flags


def between_run_cv(pps_values: Sequence[float]) -> tuple[float, float, float]:
    """Return (mean, sd, cv). Empty → (nan, nan, nan)."""
    xs = np.asarray(
        [float(x) for x in pps_values if x is not None and np.isfinite(x)],
        dtype=float,
    )
    if xs.size == 0:
        return float("nan"), float("nan"), float("nan")
    mean = float(xs.mean())
    if xs.size == 1:
        return mean, 0.0, 0.0
    sd = float(xs.std(ddof=1))
    cv = sd / mean if mean != 0 else float("nan")
    return mean, sd, cv


def mean_t_ci(
    values: Sequence[float],
    *,
    alpha: float = 0.05,
) -> tuple[float, float, float]:
    """Return ``(mean, lo, hi)`` from a Student-t interval. ``n < 2`` → open CI."""
    from scipy import stats

    xs = np.asarray(
        [float(x) for x in values if x is not None and np.isfinite(x)],
        dtype=float,
    )
    if xs.size == 0:
        return float("nan"), float("nan"), float("nan")
    mean = float(xs.mean())
    if xs.size < 2:
        return mean, float("nan"), float("nan")
    half = float(stats.t.ppf(1.0 - alpha / 2.0, xs.size - 1)) * float(
        xs.std(ddof=1) / np.sqrt(xs.size)
    )
    return mean, mean - half, mean + half


def cv_bootstrap_ci(
    pps_values: Sequence[float],
    *,
    n_boot: int = 4000,
    seed: int = 0,
    alpha: float = 0.05,
) -> tuple[float, float, float]:
    """Return ``(cv, lo, hi)``. Percentile bootstrap; ``n < 2`` leaves CI open."""
    xs = np.asarray(
        [float(x) for x in pps_values if x is not None and np.isfinite(x)],
        dtype=float,
    )
    mean, _, cv = between_run_cv(xs)
    if xs.size < 2 or not np.isfinite(cv):
        return cv, float("nan"), float("nan")
    rng = np.random.default_rng(seed)
    draws = rng.choice(xs, size=(n_boot, xs.size), replace=True)
    boot_mean = draws.mean(axis=1)
    boot_sd = draws.std(axis=1, ddof=1)
    ok = boot_mean != 0
    boot_cv = np.full(n_boot, np.nan)
    boot_cv[ok] = boot_sd[ok] / boot_mean[ok]
    boot_cv = boot_cv[np.isfinite(boot_cv)]
    if boot_cv.size == 0:
        return cv, float("nan"), float("nan")
    lo, hi = np.quantile(boot_cv, [alpha / 2.0, 1.0 - alpha / 2.0])
    return cv, float(lo), float(hi)


def summarize_variant(runs: Sequence[RunPps]) -> dict:
    valid = [r for r in runs if r.valid and r.mean_pps is not None]
    mean, sd, cv = between_run_cv([float(r.mean_pps) for r in valid])
    return {
        "n_total": len(runs),
        "n_valid": len(valid),
        "mean_pps": mean,
        "sd_pps": sd,
        "cv": cv,
        "per_run_pps": [float(r.mean_pps) for r in valid],
        "runs": valid,
    }
