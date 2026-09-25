"""Reusable PPS experiment analysis (raw counters → stats / Allan / models / figures)."""

from .allan import allan_factor, local_relative_adev, triad_rates
from .figures import figure1_summary, figure2_noise
from .io import load_counter_samples, load_runs, load_sample_reads
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
    interval_rates,
    mean_pps,
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
    "summarize_variant",
    "triad_rates",
    "local_relative_adev",
    "allan_factor",
    "FitSplit",
    "ModelResult",
    "fit_split_by_pairs",
    "compare_models_holdout",
    "detect_linear_trend",
    "simulate_poisson_counts",
    "load_runs",
    "load_counter_samples",
    "load_sample_reads",
    "figure1_summary",
    "figure2_noise",
]
