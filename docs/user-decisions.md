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
The 2026-11-25 execution authorization supersedes this entry.

## 2026-11-25 Release intake and deployment identity

The Agent fetches and validates `libbpf-mcu v0.1.1` release archives.
Use the v3 program index and pinned asset digests.
Switch artifact validation and caching from `xpost-artifact` to `xpost-agent`.
Remove the v1 resolver and sibling profile cache reuse.
Keep the firmware and partition checks in each exclusive cold installation session.

Reason: the new taskbook assigns release intake to the Agent. It keeps the verified-byte write contract from PR #24.

## 2026-11-25 Formal case and medium scope

The formal medium plan contains 29 cells.
Run one AOT Trace case cell. It measures one A-to-B publish.
Keep both policy probes, cold installation, and SRAM preload in preparation.
Keep the qualification run outside the formal matrix.
Save the raw packet capture and switch Trace with a shared run ID.
Retire the cold case timing and separate `hot_swap` selection.
Retain their historical results.

Reason: the revised brief uses one observed switch to show the dataplane and control timeline.

## 2026-11-25 Medium execution authorization

The maintainer authorizes the 29-cell run after implementation and read-only preflight pass.
Run device qualification within the medium plan.
Keep case preparation outside its timed window.
Publish each terminal attempt to PR #23 after its measurement window.
Use the original run ID to recover a missing write or export.

Reason: the revised taskbook combines device acceptance and the formal run.

## 2026-11-25 Phased medium retest

Run HAL and AOT first. Run JIT and rBPF after publication of the first phase.
Each phase covers 14 cells and 72 planned attempts.
Order the experiments as exec_bench, firewall, just_tx, and just_drop.
Defer case, lookup, and dev for this retest.
Use a new campaign for the retest. Retain earlier runs as diagnostic evidence.

Reason. The updated PR #23 brief requires HAL and AOT results before the second phase.
The earlier 29-cell scope still records the original campaign.

## 2026-11-25 Full campaign after the phased retest

Select `all` in `xpost-test/profile.toml`.
Use one new campaign for both phases and one AOT Case switch.
Run 29 cells and 625 attempts. Keep the phased medium measurements for diagnosis.
Publish the first phase and verify all its terminal keys before the second phase.
Use the 30-second network windows and 100-batch EXB2 runs specified in the full brief.

Reason. The maintainer requested the full grid after reviewing the phased PR evidence.

## 2026-11-27 Runner database configuration

New collection writes Protocol V1 to the NAS database `experiments`.
The runner reads PostgreSQL settings from `/root/.db` at runtime.
The NAS database `postgres` retains the historical data.

Reason. The maintainer stopped the historical data migration and chose a dedicated database for new collection.

## 2026-11-27 Lookup selection

Retire the standalone `lookup` selection.

Reason. The diagnostic uses the historical writer and has no equivalent Protocol V1 entry.
The maintainer chose retirement during the database refactor.

## 2026-11-27 Automatic Protocol V1 initialization

Initialize Protocol V1 when a new collection opens the empty `experiments` database.
Allow an administrator to prepare empty schemas for a writer without database-level CREATE.
Keep preflight read-only. Require existing storage when resuming a campaign.

Reason. The maintainer asked the runner to create absent Protocol V1 tables automatically.

## 2026-11-27 Store Protocol V1 in release

Use the `release` schema in `experiments` for new Protocol V1 objects.
Prefix shared objects with `bench_` and mode tables with their mode names.
Initialize the empty `release` schema. Keep preflight read-only.

Reason. The maintainer granted schema-level creation in `release` and selected that schema for new collection.
This supersedes the five-schema layout for new Protocol V1 records.
