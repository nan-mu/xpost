"""Local relative Allan deviation and Allan factor from random triads."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Sequence

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
