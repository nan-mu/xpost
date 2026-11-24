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
The 2026-11-15 schema entry adds a new protocol database. It does not replace these tables.
One `_config` table per mode and experiment, shared by both collection variants.
One `_data` table per mode, experiment, and variant.
Per-run timeline events get their own `_event` table per experiment, never a shared one.

Reason: the user asked for config and data to be separate, and for each test that designs its own data table to have its own table.

## 2026-11-11 Collection variants

Two variants: `stream` for a low-overhead rate grid, `trace` for one single run with as many timestamps as possible.
`stream` replaces the old `cag`; `trace` replaces the old `t`.
The names may change; the split must not.
The low-overhead name `stream` is superseded by the 2026-11-15 collection entry. The split is not.

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

## 2026-11-15 Four modes

`jit` is a fourth testing mode, beside `aot`, `rbpf`, and `hal`.
The 2026-11-11 names still stand. This entry adds `jit`. It does not rename the other three.
`jit` means the host compiled eBPF to RV32 machine code. The device runs that image through the same native entry as `aot`.
A manifest records `build_pipeline`. The runtime engine code does not say which pipeline built the image.
Do not use the x86-64 `rbpf` JIT for this mode. The MCU build does not include that compiler.

Reason: the xpost-experiments-v1 design adds `jit` and says to record that fact. The earlier entry did not list it.

## 2026-11-15 Collection name

The low-overhead variant is named `metrics`.
`trace` is unchanged.
A config reader may accept `stream` as an alias for `metrics`.

Reason: the same design renames `stream` to `metrics`. The 2026-11-11 entry already allowed the name to change and forbade changing the split.

## 2026-11-15 Schema for this protocol

The new protocol database has schemas `bench`, `hal`, `aot`, `jit`, and `rbpf`.
`bench` holds run identity, calibration, and raw facts.
Each mode has its own config, data, and event tables.
Do not write a `jit` result into the `aot` schema.
Do not apply this DDL on top of the existing measurement tables. The 2026-11-11 generator remains the old protocol.

Reason: the design's `sql/schema.sql` is a new database. It fails if a same-named object already exists. The old tables stay.

## 2026-11-15 Measurement library

`embeprom` is the metrics library for this protocol.
Calibrate the histogram buckets on the same board and the same path before a formal run, then freeze them.
Do not change buckets during a formal sample.

Reason: the design names `embeprom` and forbids guessed buckets in a formal run.

## 2026-11-15 Formal DROP

The formal `just_drop` program does not look up a map.
The driver or the firmware counts the drop.
The old count-then-drop program stays as a fixture.

Reason: the design separates the formal DROP program from the older map-counting fixture.

## 2026-11-15 Exec batch

The batch loop stays inside the measured program.
`batch_count` and the workload parameter are different fields.
The program enters the engine once per batch.

Reason: the 2026-11-11 batched-exec entry stands. This entry records the field split from the new design.

## 2026-11-15 HAL case

HAL case is `native_policy_select`.
It does not load an ELF, and it is not a hot load.
Its slot, rebind, and ELF fields stay null.
XPOST case publishes two slots.

Reason: the design uses HAL as a static policy control, not as another ELF loader.

## 2026-11-18 XDP artifact ownership

`libbpf-mcu` generates XDP artifacts.
`xpost-artifact` validates and caches them. This ownership is superseded by the 2026-11-25 release intake entry.
`xpost-agent` consumes existing artifacts and does not rebuild XDP.
Firmware builds remain the Agent's responsibility.

Reason: the user separated artifact production from consumption for PR #24.
Firmware SHA-256 binding and verified cache snapshots prevent installation of artifacts for another firmware.

## 2026-11-18 Artifact identity and device firmware

Sibling profile cache reuse requires matching `source_id`, `input_set_id`, and firmware SHA-256.
Profile-specific `configuration_id` values need not match.
The 2026-11-25 release intake entry supersedes these two cache rules.
Validate all producer input artifacts before cache use.
Cold XDP installation requires device application and partition-table validation in the installation session.
Unknown device identity must prevent installation.
Process-local state cannot replace device validation.

Reason: the user requested protection against stale sibling caches and mismatched device firmware in PR #24.

## 2026-11-24 Medium experiment launch

The user starts the full medium experiment from T3 after reviewing PR #23.
Readiness checks precede that launch.

Reason: the user reserved the full run for an explicit start after review.

## 2026-11-25 Release intake and deployment identity

The Agent fetches and validates `libbpf-mcu v0.1.1` release archives.
Use the v3 program index and pinned asset digests.
Switch artifact validation and caching from `xpost-artifact` to `xpost-agent`.
Remove the v1 resolver and sibling profile cache reuse.
Keep the firmware and partition checks in each exclusive cold installation session.

Reason: the new taskbook assigns release intake to the Agent. It keeps the verified-byte write contract from PR #24.
