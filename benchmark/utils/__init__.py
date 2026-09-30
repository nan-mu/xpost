"""Shared helpers for xpost/benchmark notebooks (DB + experiment analysis).

Only performance analysis: open notebooks in a standing Jupyter Lab; analysis
deps and ``utils/`` use the **pixi** env (kernelspec ``xpost-benchmark``).
Flash / stimulus live on TR3000 ``./xpost-test`` + Mac ``xpost-agent``, not here.

Layout
------
::

    benchmark/
      pixi.toml             # analysis deps; task kernel registers Jupyter kernel
      .env                  # PGSCHEMA=formal
      utils/                # db / experiment analysis (PYTHONPATH=benchmark/)
      just_drop.ipynb
      just_tx.ipynb
      tests/

Quick start
-----------
::

    # Mac (separate terminal; HTTP /health /v1/* /mcp)
    cargo xtask agent --listen 0.0.0.0:9240 --no-monitor

    # TR3000
    ./xpost-test

    # once: install deps + DDL + register kernel (for external Lab)
    cd benchmark
    cp .env.example .env   # PGSCHEMA=formal
    pixi install && pixi run ensure-schema && pixi run kernel

Jupyter Lab is standing (local install is fine; need not start via pixi)::

    jupyter lab

Open ``just_tx.ipynb`` / ``just_drop.ipynb`` with kernel **xpost-benchmark (pixi)**
(notebooks bind that name). ``utils`` imports via kernelspec ``PYTHONPATH``.

After changing pixi deps or switching machines, re-run ``pixi run kernel``.

Database
--------
=======  =================================
Var      Meaning
=======  =================================
``PG*``  connection
``PGSCHEMA``  use ``formal`` for formal perf data
=======  =================================

Tables: ``just_tx_config`` / ``just_tx_data`` (Counter sweep + RTT);
``just_tx_run`` + ``just_tx_agg`` / ``just_tx_gauge`` /
``just_tx_trace_summary`` (A/G/T fixed pps);
``just_drop_config`` / ``just_drop_data`` (Counter full-rate drop PPS),
``just_drop_run`` + ``just_drop_agg`` / ``just_drop_gauge`` /
``just_drop_trace_summary`` (A/G/T fixed pps). DDL: ``xpost-test/sql/``.

Tests
-----
::

    pixi run test
"""

from .db import connect, dsn_summary, execute, read_sql

__all__ = ["connect", "read_sql", "execute", "dsn_summary"]
