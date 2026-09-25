"""Unit tests for experiment analysis (synthetic data; no live Postgres)."""

from __future__ import annotations

import numpy as np
import pytest

from utils.experiment.allan import (
    allan_factor,
    local_relative_adev,
    overlapping_allan_factor,
    overlapping_relative_adev,
    triad_rates,
)
from utils.experiment.figures import figure1_is_single_panel, figure1_summary, figure2_noise
from utils.experiment.models import (
    compare_models_holdout,
    detect_linear_trend,
    fit_split_by_pairs,
    simulate_deterministic_counts,
    simulate_poisson_counts,
)
from utils.experiment.pps import between_run_cv, interval_rates, mean_pps, summarize_variant
from utils.experiment.schedule import generate_triad_schedule, validate_triad_dts


def test_schedule_reproducible():
    a = generate_triad_schedule(120.0, seed=42)
    b = generate_triad_schedule(120.0, seed=42)
    assert [(t.tau_s, t.t0_s, t.t1_s, t.t2_s) for t in a] == [
        (t.tau_s, t.t0_s, t.t1_s, t.t2_s) for t in b
    ]
    assert a
    # last triad must fit in window
    assert a[-1].t2_s <= 120.0


def test_validate_triad_dts():
    assert validate_triad_dts(1.0, 1.0, 1.0)
    assert not validate_triad_dts(1.2, 1.0, 1.0)  # >5%
    assert not validate_triad_dts(1.0, 1.1, 1.0)  # mutual >5% of mean roughly


def test_mean_pps_and_counter_decrease():
    t = [0, 1_000_000_000, 2_000_000_000]
    ok = mean_pps(t, [100, 200, 300], run_id="r1", variant="hal", pair_id=1)
    assert ok.valid
    assert ok.mean_pps == pytest.approx(100.0)
    bad = mean_pps(t, [100, 90, 80], run_id="r2")
    assert not bad.valid
    assert bad.reason == "counter_decrease"


def test_interval_rates_marks_decrease():
    t = [0, 1_000_000_000, 2_000_000_000]
    _, rates, flags = interval_rates(t, [10, 20, 15])
    assert flags[0] == "ok"
    assert flags[1] == "counter_decrease"
    assert np.isnan(rates[1])


def test_between_run_cv():
    mean, sd, cv = between_run_cv([100.0, 100.0, 100.0])
    assert mean == 100.0
    assert sd == 0.0
    assert cv == 0.0


def test_invalid_runs_excluded_from_summary():
    runs = [
        mean_pps([0, 1_000_000_000], [0, 100], run_id="a", variant="hal", pair_id=1),
        mean_pps([0, 1_000_000_000], [50, 40], run_id="b", variant="hal", pair_id=2),
    ]
    s = summarize_variant(runs)
    assert s["n_total"] == 2
    assert s["n_valid"] == 1
    assert s["mean_pps"] == pytest.approx(100.0)


def test_poisson_allan_factor_near_one():
    rng = np.random.default_rng(0)
    rate = 500.0
    tau = 1.0
    triads = []
    for i in range(200):
        # simulate three cumulative counts at 0, τ, 2τ
        k1 = int(rng.poisson(rate * tau))
        k2 = int(rng.poisson(rate * tau))
        tr = triad_rates([0.0, tau, 2 * tau], [0, k1, k1 + k2], triad_id=i, tau_s=tau)
        assert tr.valid
        triads.append(tr)
    af = allan_factor(triads)[tau]["af"]
    # AF≈1 for Poisson; allow sampling noise
    assert 0.7 < af < 1.3
    adev = local_relative_adev(triads, rate)[tau]["adev"]
    assert adev > 0


def test_triad_dt_tolerance_excludes_from_allan():
    # dt off by 10%
    tr = triad_rates([0.0, 1.1, 2.2], [0, 100, 200], triad_id=0, tau_s=1.0)
    assert not tr.valid
    assert tr.reason == "dt_tolerance"
    assert local_relative_adev([tr], 100.0) == {}


def test_linear_trend_detection():
    t = np.linspace(0, 100, 50)
    rates = 1000.0 * (1.0 + 0.002 * t)  # +20% over window
    d = detect_linear_trend(t, rates)
    assert d["detectable"]
    assert d["rel_change"] == pytest.approx(0.2, rel=0.05)


def test_fit_split_no_leakage():
    pairs = list(range(1, 13))
    split = fit_split_by_pairs(pairs, n_fit=8, n_holdout=4)
    assert len(split.fit_pair_ids) == 8
    assert len(split.holdout_pair_ids) == 4
    assert set(split.fit_pair_ids).isdisjoint(split.holdout_pair_ids)


def test_underdispersion_not_silent_poisson():
    dts = [1.0] * 50
    rate = 100.0
    det = simulate_deterministic_counts(dts, rate)
    pois = simulate_poisson_counts(dts, rate, seed=1)
    # deterministic should trigger underdispersion annotation
    results = compare_models_holdout(
        fit_counts=det[:40],
        fit_dts=dts[:40],
        hold_counts=det[40:],
        hold_dts=dts[40:],
        rate=rate,
    )
    names = [r.name for r in results]
    assert "underdispersion_check" in names
    # Poisson sim should usually not
    results_p = compare_models_holdout(
        fit_counts=list(pois[:40]),
        fit_dts=dts[:40],
        hold_counts=list(pois[40:]),
        hold_dts=dts[40:],
        rate=rate,
    )
    # may or may not flag; just ensure poisson is identifiable
    assert any(r.name == "poisson" and r.identifiable for r in results_p)


def test_mmpp_unidentifiable_when_small():
    dts = [1.0] * 10
    counts = simulate_poisson_counts(dts, 50.0, seed=2)
    results = compare_models_holdout(
        fit_counts=counts[:8],
        fit_dts=dts[:8],
        hold_counts=counts[8:],
        hold_dts=dts[8:],
        rate=50.0,
    )
    mmpp = next(r for r in results if r.name == "mmpp2")
    assert not mmpp.identifiable


def test_overlapping_adev_constant_rate_is_zero():
    t = [i * 1_000_000_000 for i in range(40)]
    c = [i * 1000 for i in range(40)]
    out = overlapping_relative_adev(t, c, taus_s=(1.0, 2.0, 4.0))
    assert out[1.0]["adev"] == pytest.approx(0.0, abs=1e-12)
    assert out[1.0]["n"] >= 2
    af = overlapping_allan_factor(t, c, taus_s=(1.0,))
    assert af[1.0]["af"] == pytest.approx(0.0, abs=1e-12)


def test_overlapping_adev_rejects_decrease():
    t = [0, 1_000_000_000, 2_000_000_000, 3_000_000_000]
    c = [0, 10, 5, 20]
    assert overlapping_relative_adev(t, c) == {}


def test_figure1_single_panel_two_categories():
    fig, host = figure1_summary(
        hal_mean_pps=1000.0,
        xdp_mean_pps=2000.0,
        hal_pps_points=[990, 1010],
        xdp_pps_points=[1980, 2020],
        hal_adev_1s=0.01,
        xdp_adev_1s=0.02,
        hal_cv=0.03,
        xdp_cv=0.04,
    )
    assert figure1_is_single_panel(fig)
    assert len(fig.axes) == 3
    labels = [t.get_text() for t in host.get_xticklabels()]
    assert labels == ["HAL", "XDP"]
    plt_close = __import__("matplotlib.pyplot", fromlist=["close"]).close
    plt_close(fig)


def test_figure2_shape():
    fig = figure2_noise(
        hal_adev={0.5: 0.01, 1.0: 0.02},
        xdp_adev={0.5: 0.015, 1.0: 0.025},
        hal_counts=[10, 11, 9, 10],
        xdp_counts=[20, 21, 19],
        model_scores={"hal": {"poisson": -10.0, "negbin": -9.5}, "xdp": {"poisson": -8.0}},
    )
    assert len(fig.axes) == 6
    __import__("matplotlib.pyplot", fromlist=["close"]).close(fig)
