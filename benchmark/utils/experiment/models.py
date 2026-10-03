"""Noise-model helpers: Poisson baseline, holdout split, trend detect, underdispersion."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Sequence

import numpy as np
from scipy import stats


@dataclass(frozen=True)
class FitSplit:
    fit_pair_ids: tuple[int, ...]
    holdout_pair_ids: tuple[int, ...]


@dataclass(frozen=True)
class ModelResult:
    name: str
    fit_loglik: float
    holdout_loglik: float
    identifiable: bool
    note: str = ""


def fit_split_by_pairs(
    pair_ids: Sequence[int],
    *,
    n_fit: int = 8,
    n_holdout: int = 4,
) -> FitSplit:
    """Pre-registered split: first n_fit distinct pairs for fit, next n_holdout for holdout.

    Raises if the same pair would appear in both sets.
    """
    uniq = sorted({int(p) for p in pair_ids})
    if len(uniq) < n_fit + n_holdout:
        # Still partition what we have; caller may mark evidence insufficient.
        fit = tuple(uniq[:n_fit])
        hold = tuple(uniq[n_fit : n_fit + n_holdout])
    else:
        fit = tuple(uniq[:n_fit])
        hold = tuple(uniq[n_fit : n_fit + n_holdout])
    overlap = set(fit) & set(hold)
    if overlap:
        raise ValueError(f"pair leakage between fit and holdout: {overlap}")
    return FitSplit(fit, hold)


def simulate_poisson_counts(
    dts: Sequence[float],
    rate: float,
    seed: int,
) -> np.ndarray:
    """Independent Poisson(λ Δt_i) counts for windows of length dts."""
    rng = np.random.default_rng(seed)
    lam = np.asarray(dts, dtype=float) * float(rate)
    return rng.poisson(lam)


def simulate_deterministic_counts(
    dts: Sequence[float],
    rate: float,
) -> np.ndarray:
    """Fixed-beat (underdispersed) counts: round(rate * dt). Not Poisson."""
    return np.asarray([int(round(float(rate) * float(dt))) for dt in dts], dtype=int)


def poisson_loglik(counts: Sequence[int], dts: Sequence[float], rate: float) -> float:
    lam = np.asarray(dts, dtype=float) * float(rate)
    k = np.asarray(counts, dtype=float)
    # guard zeros
    lam = np.clip(lam, 1e-300, None)
    return float(np.sum(stats.poisson.logpmf(k, lam)))


def negbin_loglik(
    counts: Sequence[int],
    dts: Sequence[float],
    rate: float,
    alpha: float,
) -> float:
    """NB with mean μ=λΔt and Var = μ + α μ² (alpha≥0). alpha→0 ≈ Poisson."""
    mu = np.asarray(dts, dtype=float) * float(rate)
    k = np.asarray(counts, dtype=float)
    mu = np.clip(mu, 1e-300, None)
    if alpha <= 1e-12:
        return poisson_loglik(counts, dts, rate)
    # scipy nbinom: n, p with mean = n(1-p)/p → set n=1/alpha, p = n/(n+mu)
    n = 1.0 / alpha
    p = n / (n + mu)
    return float(np.sum(stats.nbinom.logpmf(k, n, p)))


def trend_poisson_loglik(
    counts: Sequence[int],
    t_start: Sequence[float],
    t_end: Sequence[float],
    r0: float,
    drift: float,
) -> float:
    """Non-homogeneous Poisson: rate(t)=r0*(1+drift*t), integrate over [t0,t1]."""
    ll = 0.0
    for k, a, b in zip(counts, t_start, t_end):
        # ∫_a^b r0 (1 + drift t) dt = r0 * ((b-a) + drift/2 (b²-a²))
        mu = float(r0) * ((b - a) + 0.5 * float(drift) * (b * b - a * a))
        mu = max(mu, 1e-300)
        ll += float(stats.poisson.logpmf(int(k), mu))
    return ll


def detect_linear_trend(
    times_s: Sequence[float],
    rates: Sequence[float],
) -> dict:
    """OLS rate ~ a + b t. Returns slope, p-value, relative end-to-end change."""
    t = np.asarray(times_s, dtype=float)
    r = np.asarray(rates, dtype=float)
    mask = np.isfinite(t) & np.isfinite(r)
    t, r = t[mask], r[mask]
    if t.size < 3:
        return {
            "slope": float("nan"),
            "pvalue": float("nan"),
            "rel_change": float("nan"),
            "detectable": False,
            "reason": "too_few",
        }
    slope, intercept, _, pvalue, _ = stats.linregress(t, r)
    r0 = intercept + slope * float(t[0])
    r1 = intercept + slope * float(t[-1])
    rel = (r1 - r0) / r0 if r0 != 0 else float("nan")
    return {
        "slope": float(slope),
        "pvalue": float(pvalue),
        "rel_change": float(rel),
        "detectable": bool(pvalue < 0.05 and abs(rel) > 0.01),
        "reason": "ok",
    }


def underdispersion_index(counts: Sequence[int], dts: Sequence[float], rate: float) -> float:
    """Var(K)/E[K] under Poisson expectation; ≪1 suggests underdispersion."""
    k = np.asarray(counts, dtype=float)
    mu = np.asarray(dts, dtype=float) * float(rate)
    if k.size < 2:
        return float("nan")
    # pooled: compare sample var of (k - mu) to mean mu
    resid = k - mu
    return float(np.var(resid, ddof=1) / max(float(mu.mean()), 1e-12))


def compare_models_holdout(
    *,
    fit_counts: Sequence[int],
    fit_dts: Sequence[float],
    hold_counts: Sequence[int],
    hold_dts: Sequence[float],
    rate: float,
    fit_t0: Sequence[float] | None = None,
    fit_t1: Sequence[float] | None = None,
    hold_t0: Sequence[float] | None = None,
    hold_t1: Sequence[float] | None = None,
    drift: float = 0.0,
    nb_alpha: float = 0.05,
) -> list[ModelResult]:
    """Score Poisson / NegBin / trend Poisson on holdout. MMPP marked unidentifiable if tiny."""
    results: list[ModelResult] = []

    p_fit = poisson_loglik(fit_counts, fit_dts, rate)
    p_hold = poisson_loglik(hold_counts, hold_dts, rate)
    results.append(ModelResult("poisson", p_fit, p_hold, True))

    nb_fit = negbin_loglik(fit_counts, fit_dts, rate, nb_alpha)
    nb_hold = negbin_loglik(hold_counts, hold_dts, rate, nb_alpha)
    results.append(ModelResult("negbin", nb_fit, nb_hold, True))

    if fit_t0 is not None and fit_t1 is not None and hold_t0 is not None and hold_t1 is not None:
        tr_fit = trend_poisson_loglik(fit_counts, fit_t0, fit_t1, rate, drift)
        tr_hold = trend_poisson_loglik(hold_counts, hold_t0, hold_t1, rate, drift)
        results.append(ModelResult("trend_poisson", tr_fit, tr_hold, True))
    else:
        results.append(
            ModelResult(
                "trend_poisson",
                float("nan"),
                float("nan"),
                False,
                "need window start/end times",
            )
        )

    # Two-state MMPP needs many triads; refuse with tiny samples.
    n = len(fit_counts)
    if n < 40:
        results.append(
            ModelResult(
                "mmpp2",
                float("nan"),
                float("nan"),
                False,
                "insufficient samples to identify two-state MMPP",
            )
        )
    else:
        # Placeholder equal to Poisson — not a real MMPP fit; flag as not identified.
        results.append(
            ModelResult(
                "mmpp2",
                p_fit,
                p_hold,
                False,
                "MMPP fit not implemented; do not treat as identified",
            )
        )

    ud = underdispersion_index(fit_counts, fit_dts, rate)
    if np.isfinite(ud) and ud < 0.3:
        # Annotate poisson result — caller should not silently call it Poisson.
        results.append(
            ModelResult(
                "underdispersion_check",
                float("nan"),
                float("nan"),
                True,
                f"var/mean≈{ud:.3f}; do not classify as Poisson",
            )
        )
    return results
