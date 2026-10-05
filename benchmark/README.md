# benchmark — analysis stance (read first)

This directory is **not** an experiment-centric Evaluation workspace.

## Decision

- **Do not** revive per-experiment notebooks (`just_tx.ipynb` / `just_drop.ipynb`) or
  “one notebook per formal run” analysis. That shape steers Evaluation toward
  describing *experiments*, not *xpost features*.
- **Do** treat `formal.*` tables as raw evidence for **metric / problem** questions
  (e.g. what `hal_agg` / `xdp_agg` names mean under Pass vs Drop vs TX), often
  **across** just_tx and just_drop (and later exams).
- Same wire/SQL names on HAL vs XDP are **not** automatically the same quantity:
  compare instrumentation brackets and path contents before comparing means.
- Near-term bar: finish remaining formal experiments; use data only for a light
  sanity check (instrumentation intact, no severe path bugs). Deep charts and
  Evaluation prose come later, aligned with Methodology chapters on xpost—not
  with experiment names.

## Keep / ignore

| Path | Role |
|------|------|
| `backup/` | Frozen archives — **do not edit or delete** |
| `feature_perf.ipynb` | 按特性指标的中文分析笔记本：ARRAY helper / HASH helper / `XDP_TX` vs HAL |
| `utils/db.py` | Thin Postgres helpers for ad-hoc queries |
| `utils/trace_flame.py` + `utils/export_*.py` | Optional flame / speedscope export |
| `pixi.toml` | Analysis Python env (when needed) |
| `.env.example` | `PG*` + `PGSCHEMA=formal` |

Deleted on purpose: experiment notebooks, `utils/experiment/`, old schema/ingest
helpers and their tests. Do not restore them unless the team explicitly revisits
this decision.

## Data source

Acquisition stays on TR3000 `./xpost-test` → `formal` schema (`xpost-test/sql/formal.sql`).
This tree does not flash, stimulate, or own the USB session.
