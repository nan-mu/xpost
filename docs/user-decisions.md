# User decisions

Final decisions the user has made.
Read this before starting work and before acting on anything recorded here.
If a fact contradicts an entry, stop and ask the user instead of overriding it.

Append only. When a later decision replaces an earlier one about the same thing, keep the earlier entry and mark it superseded.

Ordinary facts about the code do not belong here. They expire.

---

## 2026-11-11 Mode names

Testing modes are named `aot`, `rbpf`, `hal`.
Do not call them arms.
`ebpf` and `xdp` remain accepted as aliases for `aot` in configuration files, because existing files use them and they always meant the machine-code path.

Reason: the user asked for one word per concept, and "mode" describes how the same program runs.

## 2026-11-11 Schema layout

One PostgreSQL schema per mode: `aot`, `rbpf`, `hal`.
One `_config` table per mode and experiment, shared by both collection variants.
One `_data` table per mode, experiment, and variant.
Per-run timeline events get their own `_event` table per experiment, never a shared one.

Reason: the user asked for config and data to be separate, and for each test that designs its own data table to have its own table.

## 2026-11-11 Collection variants

Two variants: `stream` for a low-overhead rate grid, `trace` for one single run with as many timestamps as possible.
`stream` replaces the old `cag`; `trace` replaces the old `t`.
The names may change; the split must not.

Reason: the user defined `cag` as collection with little effect on performance and `t` as one run collecting many timestamps for a flame graph.

## 2026-11-11 Report format

Metrics travel in the self-describing format. Each firmware declares which metrics it sends.
No fixed slot layout, and no Cargo feature that decides the metric set.

Reason: the user asked for each test to design its own metrics.

## 2026-11-11 Batched exec measurement

The batch loop runs inside eBPF. The host sends a parameter block that names the batch count and a result region.
PREVAIL's limits are the only limits. When a program passes PREVAIL, every other limit is raised to fit.
Measure one batch round first; more batch shapes come later.

Reason: the user wants to remove the per-call constant cost from the measurement, and named PREVAIL as the authority.

## 2026-11-11 Measurement requirements

A measurement must be comparable across modes and must reduce constant cost.
Every reported number must name its mode.

Reason: the user's two requirements, and a report without the mode was unreadable.
