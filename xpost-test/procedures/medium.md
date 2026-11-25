# Medium experiment

The default `run = ["medium"]` starts with read-only readiness checks.
The maintainer authorizes the start after implementation and preflight pass.
The 29-cell run includes the device acceptance checks.
TR3000 runs `./xpost-test` without arguments.
The script for a formal run is `./xpost-test/scripts/xpost-test.fish`.
The runner stops before device writes if a readiness check fails.
Network attribution must qualify each planned Trace and DROP Metrics cell.

## Deployment preflight

The Agent pins [libbpf-mcu v0.1.1](https://github.com/nan-mu/libbpf-mcu/releases/tag/v0.1.1) at `07a4ea9a81a7fedf7bef2d6ced8d7fcfcccfd113`.
The independent inventory checked 36 deployments across 12 archives.
Medium preflight checked 32 program/mode combinations and prepared 41 XDP ELF targets.
The 41 targets include separate Metrics and Trace Firmware ELFs for the network programs.
Medium preflight also checked four HAL Firmware ELFs.
The [v0.1.0 ledger](libbpf-mcu-v0.1.0.sha256) records the earlier release with zero deployments.

Provide the Wi-Fi build inputs for each network example before building its Firmware ELF.
Use the local `.env` paths described in the [README](../../README.md).
Run these checks from the repository root on the Mac.

```bash
cargo run --manifest-path xpost-agent/Cargo.toml -- preflight
cargo run --manifest-path xpost-agent/Cargo.toml -- preflight --inventory
cargo run --manifest-path xpost-agent/Cargo.toml -- preflight --xdp just_tx
cargo run --manifest-path xpost-agent/Cargo.toml -- preflight --xdp case_mark_idle
```

Preflight can fetch archives. It uses the Agent release cache.
The command checks existing board builds and opens no device session.
It lists each target that lacks a matching linked firmware or deployment.
The output separates the release SHA-256, prepared XDP ELF SHA-256, and image payload SHA-256.

The network and case carriers declare 3584-byte slots.
Exec has a 12288-byte SRAM slot and a 16384-byte cold storage capacity.
The released JIT payloads for `log2`, `bsort`, and `strcmp` are 9300, 10264, and 9348 B.
The published native images link at `0x40380200` and use helper `0x40380000`.
The pinned case AOT payloads also match direct links at B (`0x40381000`).
The Agent checks the payload SHA-256, empty map list, helper, and both 3584-byte slots before it prepares B.
Preparation changes the target address and alias. It keeps the payload bytes.
The case JIT and rBPF profiles remain available for independent diagnostics.
Other native slot changes require equivalent evidence or relocation data.

## Agent session

Start the service from `xpost-agent/` on the Mac.

```sh
cargo run --release --locked -- serve --bind 0.0.0.0:9240
```

Keep the execution session open through the experiment.
Press Ctrl+C after the runner finishes.
See [Agent operation](../../xpost-agent/README.md) for the HTTP endpoint and startup check.

## Planned matrix and time budget

Set `run = ["medium-plan"]` to print the 29 cells and their firmware groups.
Set `run = ["medium-preflight"]` for the read-only readiness check on TR3000.
It checks Agent health, deployment identities, protocol tables, and collector availability.
Set `run = ["medium"]` for the formal selection.
Set `medium_resume_id` to the original campaign UUID to resume that campaign.
The runner skips completed attempts and holds case when publish evidence needs recovery.

| Family | Metrics | Trace | Conditions |
| --- | ---: | ---: | --- |
| firewall | 4 | 4 | 400, 1400, 4000 pps, two 5 s repeats. Trace uses one hit and one miss. |
| just_drop | 4 | 4 | Same rates and repeats. Trace uses one tagged probe. |
| just_tx | 4 | 4 | Same rates and repeats. Trace uses one JTX1 probe. |
| exec_bench | 4 | 0 | Seven programs. One condition and two batches per program and mode. |
| case | 0 | 1 | AOT Trace. One measured A-to-B publish. |
| Total | 16 | 13 | 29 required cells |

`lookup` and `dev` run separately from the medium plan.
AOT and JIT can share a native Firmware ELF.
Group execution by firmware digest. Keep rates and repeats within the group.
Count firmware flashes, cold installs, SRAM preloads, preparation publishes, and measured publishes.
The case preparation plans one firmware flash, two cold installs, two preloads, and two publishes.
The formal case window then measures one publish. These counts need device verification.

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
  prepare: reserve a collection epoch
  arm: obtain baseline S0 and open the observation window
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
The case Trace firmware implements the five session callbacks.
The network and EXB2 carriers use embeprom 0.3.0 counters.
Network Trace records probe identity when the firmware can read the tag.
The runner checks the scope of DUT counts before it attributes them to a stimulus.

- **Firewall.** Bind once per mode. Before each window, reset known hit and miss keys.
  Pace mixed traffic for Metrics. Send one tagged hit or miss per Trace window.
  After drain, compare unique legal tokens and blacklist deltas.
  Legal delivery divides matched legal tokens by successful legal sends.
  Divide the targeted blacklist-key delta by successful illegal sends.
  Record hits on other keys separately. Reject changed keys or decreasing counters.
  Keep ratios above one for investigation. Return NULL for a zero denominator.
  Mark a window inconclusive when the DUT observes neither traffic class.
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
- **Case.** Prepare A with `case_swap` and B with `case_mark_idle`.
  Send 40-byte UDP probes with JTX1 tags and an eight-byte zero tail.
  Check version 2 for A and version 1 for B. Check the complete reply frame.
  Restore A as active and B as staged. Arm one Trace epoch and publish B once.
  Keep the tcpdump capture tail after the first valid B reply.
  Store the original pcap, Trace events, switch reply, and preparation under one run ID.
  Plot packet capture times and device Trace ticks in their own clock domains.
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

Set `[postgres].protocol_dbname` in `xpost-test/config/default.toml`.
Build and upload the selected runner with `./xpost-test/scripts/xpost-test.fish`.
If the case database write fails, the runner exports available evidence under `case-evidence/<run_id>`.
Use a dedicated database with the [four-mode schema](../sql/protocol_v1.sql).
Apply the schema once to an empty database.
The existing database holds earlier measurements.
The case entry checks the protocol database before the firmware flash.

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

Read each `XPOST_RESULT` line as an attempt ends.
Query its run ID and update the English PR while independent attempts continue.
On a database write failure, query the original run ID before attempting recovery.
Preserve failed attempts. Give each retry its own run ID.
Update PR #23 in English with the analysis, fixture scope, and retry evidence.
Use actual runs for the PR figures. Report missing and unqualified samples explicitly.
Report total wall time, extra time for diagnosis and retries, and the four device action counts.
