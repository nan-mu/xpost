"""Random three-point sampling schedule (τ ∈ {0.5,1,2,4,8,16} s)."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Sequence

import numpy as np

TAU_CANDIDATES: tuple[float, ...] = (0.5, 1.0, 2.0, 4.0, 8.0, 16.0)
_DT_TOL = 0.05  # 5% vs target τ and between the two windows


@dataclass(frozen=True)
class Triad:
    triad_id: int
    tau_s: float
    t0_s: float
    t1_s: float
    t2_s: float
    wait_after_s: float


def generate_triad_schedule(
    window_s: float,
    seed: int,
    *,
    taus: Sequence[float] = TAU_CANDIDATES,
    wait_max_s: float = 2.0,
) -> list[Triad]:
    """Draw triads until the next group would exceed `window_s`.

    Each triad: pick τ uniformly, read at t, t+τ, t+2τ, then wait U[0, wait_max_s].
    """
    rng = np.random.default_rng(seed)
    taus = tuple(float(t) for t in taus)
    out: list[Triad] = []
    t = 0.0
    tid = 0
    while True:
        tau = float(rng.choice(taus))
        need = 2.0 * tau
        if t + need > window_s:
            break
        wait = float(rng.uniform(0.0, wait_max_s))
        out.append(
            Triad(
                triad_id=tid,
                tau_s=tau,
                t0_s=t,
                t1_s=t + tau,
                t2_s=t + 2.0 * tau,
                wait_after_s=wait,
            )
        )
        tid += 1
        t = t + need + wait
    return out


def validate_triad_dts(
    dt1: float,
    dt2: float,
    tau_s: float,
    *,
    tol: float = _DT_TOL,
) -> bool:
    """True iff both windows are within `tol` of τ and of each other."""
    if tau_s <= 0 or dt1 <= 0 or dt2 <= 0:
        return False
    if abs(dt1 - tau_s) / tau_s > tol:
        return False
    if abs(dt2 - tau_s) / tau_s > tol:
        return False
    mean_dt = 0.5 * (dt1 + dt2)
    if mean_dt <= 0:
        return False
    return abs(dt1 - dt2) / mean_dt <= tol
