"""Reusable PPS experiment analysis (raw counters → stats / Allan / models / figures)."""

from .allan import (
    allan_factor,
    local_relative_adev,
    mean_across_runs,
    overlapping_allan_factor,
    overlapping_relative_adev,
    triad_rates,
)
from .figures import (
    figure1_summary,
    figure1_tx_rtt,
    figure2_noise,
    figure_tx_pps_mean,
    figure_tx_rtt_boxplot,
    figure_tx_rtt_loss_vs_rate,
    figure_tx_rtt_return_pps,
)
from .io import (
    load_counter_samples,
    load_just_tx_rtt_runs,
    load_just_tx_rtt_samples,
    load_runs,
    load_sample_reads,
)
from .models import (
    FitSplit,
    ModelResult,
    compare_models_holdout,
    detect_linear_trend,
    fit_split_by_pairs,
    simulate_poisson_counts,
)
from .pps import (
    RunPps,
    between_run_cv,
    cv_bootstrap_ci,
    interval_rates,
    mean_pps,
    mean_t_ci,
    summarize_variant,
)
from .schedule import TAU_CANDIDATES, Triad, generate_triad_schedule, validate_triad_dts

__all__ = [
    "TAU_CANDIDATES",
    "Triad",
    "generate_triad_schedule",
    "validate_triad_dts",
    "RunPps",
    "mean_pps",
    "interval_rates",
    "between_run_cv",
    "cv_bootstrap_ci",
    "mean_t_ci",
    "summarize_variant",
    "triad_rates",
    "local_relative_adev",
    "overlapping_relative_adev",
    "overlapping_allan_factor",
    "mean_across_runs",
    "allan_factor",
    "FitSplit",
    "ModelResult",
    "fit_split_by_pairs",
    "compare_models_holdout",
    "detect_linear_trend",
    "simulate_poisson_counts",
    "load_runs",
    "load_just_tx_rtt_runs",
    "load_just_tx_rtt_samples",
    "load_counter_samples",
    "load_sample_reads",
    "figure1_summary",
    "figure1_tx_rtt",
    "figure_tx_pps_mean",
    "figure_tx_rtt_loss_vs_rate",
    "figure_tx_rtt_boxplot",
    "figure_tx_rtt_return_pps",
    "figure2_noise",
]
