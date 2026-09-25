"""Local relative Allan deviation and Allan factor from random triads."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Mapping, Sequence

import numpy as np

from .schedule import validate_triad_dts


@dataclass(frozen=True)
class TriadRate:
    triad_id: int
    tau_s: float
    dt1: float
    dt2: float
    r1: float
    r2: float
    k1: int
    k2: int
    valid: bool
    reason: str = ""


def triad_rates(
    times_s: Sequence[float],
    counters: Sequence[int],
    *,
    triad_id: int,
    tau_s: float,
) -> TriadRate:
    """Three snapshots → two adjacent rates. Invalid if spacing or monotonicity fails."""
    if len(times_s) != 3 or len(counters) != 3:
        return TriadRate(triad_id, tau_s, 0, 0, 0, 0, 0, 0, False, "need_three")
    t0, t1, t2 = (float(x) for x in times_s)
    c0, c1, c2 = (int(x) for x in counters)
    dt1, dt2 = t1 - t0, t2 - t1
    if c1 < c0 or c2 < c1:
        return TriadRate(triad_id, tau_s, dt1, dt2, 0, 0, 0, 0, False, "counter_decrease")
    if not validate_triad_dts(dt1, dt2, tau_s):
        return TriadRate(triad_id, tau_s, dt1, dt2, 0, 0, 0, 0, False, "dt_tolerance")
    k1, k2 = c1 - c0, c2 - c1
    return TriadRate(
        triad_id, tau_s, dt1, dt2, k1 / dt1, k2 / dt2, k1, k2, True
    )


def local_relative_adev(
    triads: Sequence[TriadRate],
    mean_rate: float,
) -> dict[float, dict]:
    """A(τ) = (1/r̄) √(½ M⁻¹ Σ (r₂−r₁)²) over valid triads per τ."""
    by_tau: dict[float, list[TriadRate]] = {}
    for tr in triads:
        if not tr.valid:
            continue
        by_tau.setdefault(tr.tau_s, []).append(tr)

    out: dict[float, dict] = {}
    for tau, xs in sorted(by_tau.items()):
        m = len(xs)
        if m == 0 or not np.isfinite(mean_rate) or mean_rate == 0:
            out[tau] = {"adev": float("nan"), "n": m, "reason": "insufficient"}
            continue
        diffs = np.asarray([(x.r2 - x.r1) ** 2 for x in xs], dtype=float)
        adev = float(np.sqrt(0.5 * diffs.mean()) / mean_rate)
        out[tau] = {"adev": adev, "n": m, "reason": "ok"}
    return out


def overlapping_relative_adev(
    time_ns: Sequence[int],
    counter: Sequence[int],
    *,
    taus_s: Sequence[float] = (1.0, 2.0, 4.0, 8.0, 16.0),
) -> dict[float, dict]:
    """Overlapping two-sample relative ADEV on a nearly regular counter series.

    For stride ``m`` (τ ≈ m × median Δt)::

        r_i = (C_{i+m} − C_i) / (t_{i+m} − t_i)
        A(τ) = r̄⁻¹ √( mean_i (r_{i+m} − r_i)² / 2 )

    ``r̄`` is the endpoint PPS of this series. This is the fixed-cadence
    overlapping estimator, not the random-triad local estimator.
    A decrease, a non-positive gap, or fewer than two pairs returns ``{}``.
    """
    t = np.asarray(time_ns, dtype=float) / 1e9
    c = np.asarray(counter, dtype=float)
    n = int(min(t.size, c.size))
    if n < 4:
        return {}
    t, c = t[:n], c[:n]
    dt = np.diff(t)
    dc = np.diff(c)
    if np.any(dt <= 0) or np.any(dc < 0):
        return {}
    med = float(np.median(dt))
    elapsed = float(t[-1] - t[0])
    if med <= 0 or elapsed <= 0:
        return {}
    mean_rate = float((c[-1] - c[0]) / elapsed)
    if mean_rate <= 0:
        return {}

    out: dict[float, dict] = {}
    for tau in taus_s:
        m = max(1, int(round(float(tau) / med)))
        n_pairs = n - 2 * m
        if n_pairs < 2:
            continue
        span_t = t[m:] - t[:-m]
        if np.any(span_t <= 0):
            continue
        rates = (c[m:] - c[:-m]) / span_t
        diffs = rates[m:] - rates[:-m]
        adev = float(np.sqrt(np.mean(diffs**2) / 2.0) / mean_rate)
        out[float(tau)] = {
            "adev": adev,
            "n": int(diffs.size),
            "m": m,
            "tau_s": m * med,
            "reason": "ok",
        }
    return out


def overlapping_allan_factor(
    time_ns: Sequence[int],
    counter: Sequence[int],
    *,
    taus_s: Sequence[float] = (1.0, 2.0, 4.0, 8.0, 16.0),
) -> dict[float, dict]:
    """Overlapping Allan factor. A constant-rate series is 0; Poisson is near 1.

    AF(τ) = E[(K_{i+m} − K_i)²] / (2 E[K]), with K_i = C_{i+m} − C_i.
    """
    t = np.asarray(time_ns, dtype=float) / 1e9
    c = np.asarray(counter, dtype=float)
    n = int(min(t.size, c.size))
    if n < 4:
        return {}
    t, c = t[:n], c[:n]
    dt = np.diff(t)
    dc = np.diff(c)
    if np.any(dt <= 0) or np.any(dc < 0):
        return {}
    med = float(np.median(dt))
    if med <= 0:
        return {}

    out: dict[float, dict] = {}
    for tau in taus_s:
        m = max(1, int(round(float(tau) / med)))
        n_pairs = n - 2 * m
        if n_pairs < 2:
            continue
        counts = c[m:] - c[:-m]
        diffs = counts[m:] - counts[:-m]
        ek = float(np.mean(counts))
        if ek <= 0:
            continue
        out[float(tau)] = {
            "af": float(np.mean(diffs**2) / (2.0 * ek)),
            "n": int(diffs.size),
            "m": m,
            "tau_s": m * med,
            "reason": "ok",
        }
    return out


def mean_across_runs(
    per_run: Sequence[Mapping[float, float]],
    *,
    alpha: float = 0.05,
) -> dict[float, dict]:
    """Per-τ mean and Student-t interval across runs. ``n < 2`` leaves CI open."""
    from scipy import stats

    taus: set[float] = set()
    for row in per_run:
        taus.update(float(k) for k in row)
    out: dict[float, dict] = {}
    for tau in sorted(taus):
        vals = np.asarray(
            [float(row[tau]) for row in per_run if tau in row and np.isfinite(row[tau])],
            dtype=float,
        )
        if vals.size == 0:
            continue
        mean = float(vals.mean())
        if vals.size < 2:
            lo = hi = float("nan")
        else:
            half = float(stats.t.ppf(1.0 - alpha / 2.0, vals.size - 1)) * float(
                vals.std(ddof=1) / np.sqrt(vals.size)
            )
            lo, hi = mean - half, mean + half
        out[tau] = {"mean": mean, "lo": lo, "hi": hi, "n": int(vals.size), "values": vals}
    return out


def allan_factor(triads: Sequence[TriadRate]) -> dict[float, dict]:
    """AF(τ) = E[(K₂−K₁)²] / (2 E[K]) for valid equal-τ windows."""
    by_tau: dict[float, list[TriadRate]] = {}
    for tr in triads:
        if not tr.valid:
            continue
        by_tau.setdefault(tr.tau_s, []).append(tr)

    out: dict[float, dict] = {}
    for tau, xs in sorted(by_tau.items()):
        ks = []
        d2 = []
        for x in xs:
            ks.extend([x.k1, x.k2])
            d2.append((x.k2 - x.k1) ** 2)
        ek = float(np.mean(ks)) if ks else float("nan")
        if not np.isfinite(ek) or ek == 0:
            out[tau] = {"af": float("nan"), "n": len(xs), "reason": "insufficient"}
        else:
            out[tau] = {"af": float(np.mean(d2) / (2.0 * ek)), "n": len(xs), "reason": "ok"}
    return out
