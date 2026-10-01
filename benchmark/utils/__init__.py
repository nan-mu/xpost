"""Thin helpers for ad-hoc formal DB access (not experiment notebooks).

See ``benchmark/README.md`` for analysis stance: metric/problem-centric, not
per-experiment Evaluation notebooks.
"""

from .db import connect, dsn_summary, execute, read_sql

__all__ = ["connect", "read_sql", "execute", "dsn_summary"]
