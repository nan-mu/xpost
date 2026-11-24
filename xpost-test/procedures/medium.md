# Medium experiment

**The medium experiment remains blocked before device writes.**

The default `run = ["medium"]` requests a read-only Agent preflight.
The user starts the full run from T3 after review and readiness checks.
TR3000 runs `./xpost-test` without arguments.
The script for a formal run is `./xpost-test/scripts/xpost-test.fish`.
The medium runner still needs EXB2 transport, firmware perf callbacks, embeprom, the scheduler, and the four-mode writer.

## Deployment preflight

The Agent pins [libbpf-mcu v0.1.1](https://github.com/nan-mu/libbpf-mcu/releases/tag/v0.1.1) at `07a4ea9a81a7fedf7bef2d6ced8d7fcfcccfd113`.
The Agent checked 12 archives and 36 deployments. It prepared 37 of 39 slot targets.
The [v0.1.0 ledger](libbpf-mcu-v0.1.0.sha256) records the earlier release with zero deployments.

Run these checks from the repository root on the Mac.

```bash
cargo run --manifest-path xpost-agent/Cargo.toml -- preflight
cargo run --manifest-path xpost-agent/Cargo.toml -- preflight --xdp just_tx
cargo run --manifest-path xpost-agent/Cargo.toml -- preflight --xdp case_mark_idle
cargo run --manifest-path xpost-agent/Cargo.toml -- preflight --xdp case_mark_idle_rbpf
```

Preflight can fetch archives. It uses the Agent release cache.
The command checks existing board builds and opens no device session.
It lists each target that lacks a matching linked firmware or deployment.
The output separates the release SHA-256, prepared XDP ELF SHA-256, and image payload SHA-256.

The network and case carriers declare 3584-byte slots.
Exec has a 12288-byte SRAM slot and a 16384-byte cold storage capacity.
The released JIT payloads for `log2`, `bsort`, and `strcmp` are 9300, 10264, and 9348 B.
The published native images link at `0x40380200` and use helper `0x40380000`.
AOT and JIT need relocation information for another address.
The Agent refuses a native slot mismatch before it passes an operation to the serial worker.
The `case_mark_idle` and `case_mark_idle_jit` profiles need slot B at `0x40381000`.
The pinned release provides these native payloads for slot A only.
The `case_mark_idle_rbpf` profile passed host preparation for slot B with the original payload digest.
The QEMU evidence for three relocated AOT samples used linked inputs with retained relocation records.
It does not cover these published native payloads.
The rBPF release uses map indexes. Its SRAM slot may change without rewriting its instructions.

## Planned matrix and time budget

Every family has HAL, AOT, JIT, and rBPF conditions.
Metrics and Trace are separate collection variants.

| Family | Required Metrics | Required Trace | Optional Trace | Medium density |
| --- | --- | --- | --- | --- |
| firewall | 4 | 4 | 0 | 400, 1400, 4000 pps, two 5 s repeats. One hit and one miss Trace per mode. |
| just_drop | 4 | 4 | 0 | Same rates and window. One tagged Trace probe per mode. |
| just_tx | 4 | 4 | 0 | Same rates and window. One JTX1 Trace probe per mode. |
| exec_bench | 4 | 0 | 4 | All seven programs. One condition and two batches per program and mode. |
| case | 4 | 4 | 0 | Check both policies. Then perform four Metrics switches and one Trace switch per mode. |
| Total | 20 | 16 | 4 | 36 required combinations and 4 optional combinations |

The case family includes `case` and `hot_swap`.
Both procedures use `experiment = 'case'`.
Store the procedure in the condition parameters.
Store hot-switch details in the mode's `case_switch` table.
The two procedures contribute observations to the same matrix cell.
Exclude `dev` and `lookup` from the matrix.
Keep `formal-clear` outside the medium queue.

The estimates assume one firmware per family for each of HAL, native, and interpreter.
AOT and JIT share a native firmware when their identities match.
Group work by firmware digest and process each group consecutively.
Coincident firmware identities can reduce the number of writes.

| Device action | Planned bound | Observed |
| --- | ---: | ---: |
| Firmware flash | 15 | 0 |
| XDP ELF cold install | 36 | 0 |
| SRAM preload | 36 | 0 |
| Leased slot publish | 24 | 0 |

The plan assumes one cold install and at most one preload for each required deployment.
The case portion budgets eight policy selections per mode, including up to three setup selections and five measured switches.
HAL can perform eight static selections. They use no XDP slots.
Adding rates or batch repeats does not increase the firmware flash count.
Count retries separately and check device identity before each retry.

| Work | Budget |
| --- | ---: |
| Normal deployment, waits, and state checks | 18 min |
| Network stimuli, drain, and export | 12 min |
| EXB2 invocation and export | 8 min |
| Case stimuli, switches, and export | 8 min |
| Per-experiment PostgreSQL checks and analysis | 10 min |
| Reconciliation and reporting | 4 min |
| Total | 60 min |

The network plan contains 72 Metrics windows and 6 minutes of paced sends.
These are budget allocations. A device run must check the total wall time and operation counts.

## Procedure and result criteria

```text
before any device write:
  check all released deployments, firmware bindings, and slot capacities
  check the dedicated PostgreSQL database and its four-mode schema
  check target identity and support for required callbacks
for each firmware group:
  check device identity and flash only when it differs
  run all modes and collection variants served by the firmware
  keep installs outside the sample loop except when the sample measures them
  prepare: obtain baseline S0
  arm: open the observation window
  run the stimulus, then drain
  freeze: wait for observations in flight and obtain S1
  read: export the selected variant
  release: release the session
  save the attempt and query its run_id
```

Use the same boot, epoch, profile, and series for Counter and Histogram deltas.
Read Gauge snapshots directly.
Keep one board-specific frozen histogram calibration per comparison boundary.
Record the tick rate and clock domain for each clock.
The five firmware callbacks and embeprom 0.3.0 integration remain open.
Their RV32IMC atomic, interrupt, static memory, and epoch checks remain open.

- **Firewall.** Bind once per mode. Before each window, reset known hit and miss keys.
  Pace mixed traffic for Metrics. Send one tagged hit or miss per Trace window.
  After drain, compare unique legal tokens and blacklist deltas.
  Legal delivery divides matched legal tokens by successful legal sends.
  Illegal hit ratio divides the blacklist delta by successful illegal sends.
  Keep ratios above one for investigation. Return NULL for a zero denominator.
- **Just drop.** Bind the minimal DROP policy once. Check connectivity and pin the neighbor before enabling DROP.
  Send tagged probes. Freeze after drain. Separate normal DROP from runtime faults.
  Divide successful sends by the monotonic host send interval for offered pps.
  Divide attributed ingress and completed-drop counts by the DUT window.
  Use socket discard on HAL and completed XDP DROP on XPOST.
  Mark unattributable device counts inconclusive.
- **Just TX.** Start TX/RX capture and snapshot `rxcnt` before arming the window.
  Send JTX1 with run UUID and sequence. Wait for bounded drain and freeze.
  Compare the full returned frame. Count unique valid replies, duplicates, late replies, and corrupt replies.
  Use successful sends for the loss denominator. Qualify loss only with valid capture and clock evidence.
  Use unique TX and first valid RX timestamps on the same clock for RTT.
- **EXB2.** Select the static HAL entry or install the required XDP ELF once per program and mode.
  Prepare one 8258-byte buffer per sample. Call the program once with the full buffer.
  The program executes every round internally. Observe the batch outside the call.
  Record XDP action or direct-entry status separately from the buffer status.
  Check completed rounds, result, digest, input, and padding.
  Check every scratch byte for copy and bsort.
  Report ticks per round only for a correct batch measured on the DUT clock.
- **Case.** Select each static HAL policy or activate each prepared slot in turn.
  Check `case_swap` and `case_mark` against their separate expected output frames.
  Disable the policy before you prepare a baseline. Arm, send tagged probes, drain, and freeze.
  Count each probe identity once. Keep sent, correct, wrong-version, and missing counts per policy.
- **Hot swap.** Prepare both policies once. Keep a tagged stimulus active across each measured switch.
  Preload both XPOST slots before the timed switch loop. Check that preload failure leaves the current policy active.
  Record DUT switch, quiesce, and publish ticks. Record host command and reply times separately.
  Record the interval from the last old reply to the first new reply, old replies after a new reply, and missing probes.
  HAL uses static selection and leaves slot and rebind fields null.
- **Lookup.** Use explicit diagnostics. Pair just_tx runs at 1400 pps with timing off and on.
  For firewall, capture separate hash hit and miss buckets at 400, 1400, and 4000 pps.
  Keep zero samples distinct from zero duration. The minimal DROP program makes no map lookup.
- **Dev.** Run HAL smoke with short, known inputs. Check the dataplane result.
  Leave the formal database untouched.

## EXB2 qualification vectors

All rows use three rounds, seed 1, variant 0, a 64-byte little-endian header, and a total capacity of 8258 bytes.
Initialize scratch with `a5` and remaining padding with `cc`.
Set request status to `0xffffffff`.
In the host check, the published C direct entry returned 0 for all rows.
Every response preserved the input and padding and reported status 0 and completed 3.
An XDP execution should return `XDP_PASS` (2) on success or `XDP_ABORTED` (0) on error.

| Task | n | Input | Result low | Result high | Digest | Scratch output |
| --- | ---: | --- | --- | --- | --- | --- |
| noop | 0 | empty | 0 | 0 | `91efd7d5` | — |
| prime | 19 | empty | 8 | 19 | `91efb91a` | — |
| log2 | 4 | u32 values 0, 1, 4, 255 | 9 | `0000048a` hex | `91fe55a4` | — |
| copy | 4 | hex `44 20 82 3c` | 4 | 0 | `91efe859` | hex `44 20 82 3c` |
| fletcher32 | 3 | hex `01 02 03` | `04050204` hex | 0 | `199d264a` | — |
| bsort | 4 | u32 values 8, 2, 1, 8 | 1 | 8 | `91efbaee` | u32 values 1, 2, 8, 8 |
| strcmp | 4 | `abcz\0abdz\0` | `ffffffff` hex | 3 | `664ce84e` | — |

The 88-byte `workload_v2` design had no runtime caller at the review head.
The published ABI replaces that design.
The older 32-byte `workload` path remains reachable from explicit legacy selections.
It still needs replacement in the runner and firmware.

## Database and review

Set the medium PostgreSQL address and database name under `[postgres]` in `xpost-test/config/default.toml` after the writer supports the new schema.
Use a dedicated empty database with the [four-mode schema](../sql/protocol_v1.sql).
The existing database holds earlier measurements.
The current readiness check does not connect to either database.

```bash
psql -X -v ON_ERROR_STOP=1 -f xpost-test/sql/protocol_v1.sql <new-database>
psql -X -v ON_ERROR_STOP=1 -v run_id=<uuid> -f xpost-test/sql/medium_run.sql <new-database>
```

The read query selects the mode table for the run and returns the condition, config, result, and artifact identities.
It also returns the Metrics snapshots or Trace events and available experiment-specific observations.
Read the original bytes from `bench.artifact.original_bytes` with the returned artifact ID.
The SQL fixture check wrote EXB2 rows for all four modes and read each run with this query.
The check also exercised the network and case query paths with blocked fixture rows.
A second DDL apply rejected the existing objects.
These checks used a disposable PostgreSQL instance.

After each hardware attempt, analyze its run ID before advancing the queue.
Preserve failed attempts. Give each retry its own run ID.
Update PR #23 in English with the analysis, fixture scope, and retry evidence.
Use actual runs for the PR figures. Report missing and unqualified samples explicitly.
Report total wall time, extra time for diagnosis and retries, and the four device action counts.
