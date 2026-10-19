# xpost

The `xpost` crate provides an XDP runtime for Rust MCU applications.
This repository also contains board examples and host tools.
The Phase 1 target is ESP32-C3 with RV32IMC.

## Artifact names

Use these names in documents and logs:

- **Firmware ELF**: The board application.
  Network examples initialize Wi-Fi. The `exec_bench` examples do not initialize Wi-Fi.
  XDP firmware includes `XdpRt`.
  HAL firmware has no xpost runtime.
  Typical path: `examples/<app>/.../release/<app>`.
- **XDP ELF** (`xdp.elf`): The cold-region XDP container with `.xpost.meta` and `.xpost.image` sections.
  Typical path: `../build/<stem>/<stem>.elf`.
- **`esp32c3.elf`**: The libbpf-mcu AOT debug ELF.
  Typical path: `../build/<stem>/esp32c3.elf`.
  Do not use this file for cold installation.

Use **XDP ELF** or **`xdp.elf`** instead of "Compact ELF" in documents and logs.

## Repository layout

- The root crate provides maps, helpers, IRAM slots, RX processing, and the optional `exec-call` interface.
- `esp-radio` is the local radio driver with XDP ingress and performance hooks.
- `xpost-type` provides the shared XDP actions and traits.
- `xpost-elf` provides XDP ELF encoding/decoding and the XPM1 memory-map contract.
- `xpost-macros` provides map derives and the `flash_layout!` wire-layout macro.
- The firmware applications under `examples/` have separate Cargo workspaces.
  `examples/hal_perf` is a shared library for HAL CAGT reports and map wire helpers.
  `examples/exec_bench` runs XDP images through function calls without Wi-Fi.
  `examples/exec_bench_hal` runs the native Rust comparison without the xpost runtime.
  `examples/esp32c3` is a Wi-Fi starter project, outside the formal profile registry.
- `xpost-serial` provides the serial protocol library.
- `xpost-agent` provides the host daemon.
- `xpost-build` provides build tools, including `xpost-link` and `xpost-compact-elf`.
  Preserve these executable names. Call their cold-install artifacts XDP ELFs.
- `xpost-test` controls the complete test procedure.
- `benchmark` contains Python analysis tools and a feature-oriented Markdown experiment report.

The root workspace members are `.`, `esp-radio`, `xpost-macros`, `xpost-elf`, and `xpost-type`.
It excludes `xpost-build`, `xpost-agent`, `xpost-serial`, `xpost-test`, and `examples`.
The four excluded packages have separate Cargo workspaces.
The root defaults to `riscv32imc-unknown-none-elf`.
`xpost-agent` and `xpost-serial` override it with `aarch64-apple-darwin`.
`xpost-build` has no target override.
Use an explicit host `--target` when building or testing it directly.
`xpost-trait/` contains older traits. Current workspace members and dependencies do not reference it.
Use `xpost-type`.
The `xpost-test` workspace contains `common` and `ebpf`.
The `common` package provides the ABI library and the `xpost-test` binary.
The `ebpf` package uses aya: TC egress timestamps and XDP ingress timestamps, with TC ingress as an attach fallback.
The host modules are `agent`, `config`, `db`, `pipeline`, and `exam`.

## Agent access

The `xpost-agent` daemon controls the shared USB connection.
It serves HTTP `/health` and `/v1/*`, and Streamable HTTP MCP `/mcp`, on one port.
Startup opens the USB session only.
App and flash operations run when a request arrives.

**AI agents must use MCP for direct operations on `xpost-agent`.**

1. Read the available MCP tool list, tool descriptions, and input schemas before a call.
2. Use the specified input format.
3. Check the returned result.

If MCP is unavailable, stop the affected Agent operations.
Report the reason.
Do not use HTTP or direct serial calls as an alternative.
Do not start the daemon automatically to inspect its API.
Apply these rules to cold-table operations too.
These rules cover direct AI operations.
The `xpost-test` program continues to use HTTP internally.
It does not hold the USB connection while the Agent runs.

The Mac connected to the ESP through USB hosts the daemon.
The startup command is:

```bash
cd xpost-agent && cargo run --release -- --bind 0.0.0.0:9240
```

Example Cursor MCP URL: `http://127.0.0.1:9240/mcp`.

For `flash/fw`, the Agent uses the `build` entry in `profiles.toml` before it writes the firmware.
The build runs in `examples/<app>` with `cargo build --release` and the profile's feature arguments, if any.
For profiles with `fw.xdp[]`, it then clears the cold table and builds and installs the linked XDP images.
HAL profiles have an empty or omitted `xdp` list and skip this step.
Network XDP profiles build through `pixi run build --firmware ... -c <name>` in `../libbpf-mcu`.
The exec XDP profiles use the same command with firmware `examples/exec_bench/.../exec_bench`.
Each exec name is its own `libbpf-mcu/tests/xdp/<name>.c` (incr, square, bitswap, fib, buf, memcpy, fletcher32, bsort).
Check `libbpf-mcu/make/ninja/{bpf,esp32c3}.ninja` before describing the compiler pipeline.
Current rules compile BPF and RV32IMC separately from C. The installed image comes from the RV32IMC build.
Do not describe this build as translating eBPF bytecode into RV32IMC.

Each listed network firmware project reads `WIFI_SSID` and `WIFI_PASS` from its own `.env` file:

- `examples/just_xdp/`
- `examples/firewall/`
- `examples/just_drop_hal/`
- `examples/just_tx_hal/`
- `examples/firewall_hal/`

These build scripts also accept `WIFI_PASSWORD` as an alias for `WIFI_PASS`.
The HAL build scripts allow nonempty process environment values to override `.env` credentials.

## Serial protocol

The `xpost-serial` crate is a library only.
Its shared `r#type` module defines the wire protocol.
Host tools enable the `client` feature.
Firmware enables the `server` feature, which provides `SerialHandler` and `serial_task!`.
Examples use `default-features = false, features = ["server"]`.
There are no default features.
`client` and `server` are mutually exclusive unless the build enables `dev`.
The in-memory protocol tests run from `xpost-serial/` with `cargo test --features "dev,client,server"`.

The App wire types are `xpost_serial::command::Command` and `xpost_serial::reply::OkReply`.
The App commands are:

- `ping`
- `enable xdp` and `disable xdp`
- `enable exec` and `disable exec`
- `clear map`, `get map`, and `update map`
- `clear perf` and `get perf`

A successful `get map` or `get perf` response contains an `OK get ... <n>` header and `n` blob bytes.
The map blob is an ASCII snapshot, generated by `xpost::map::dump` for XDP firmware and `hal_perf::wire` for HAL firmware.
The perf blob uses the `ext::perf` layout.
HAL reports use the compatible CAGT layout from `examples/hal_perf`.
`Command::GetInit` waits for unsolicited `OK init` on the host.
`Command::GetFinish` waits for unsolicited `OK finish` on the host.
Neither command sends a UART request.
`enable exec` runs one computation in the exec firmware.
The XDP image emits `OK finish` through `bpf_printk`.
The HAL firmware writes the same UART marker.
This line is the host wait for `GetFinish`.
It is not the workload time.
Cold installation selects an image by its XDP name.

## Test procedure

**Use a formal run by default when the user asks to test, run, or check the system.**
On TR3000, always run the binary as `./xpost-test`.
Do not add environment variables, subcommands, or CLI flags to a formal run.

The build embeds these files from `xpost-test/config/`:

- `default.toml`
- `just_tx.toml`
- `just_drop.toml`
- `firewall.toml`
- `exec_bench.toml`

To change experiment selection, arms, or firewall cases, edit these TOML files.
Rate grids, stimulus durations, and pipeline steps are constants/code in `xpost-test/common/src/exam/`.
Change that code when changing those settings.
Rebuild the binary.
Upload it before the next run.

Use the repository fish script from the repository root:

```bash
./xpost-test/scripts/xpost-test.fish
```

The script builds the binary, uploads it to TR3000, and runs it without arguments through SSH.
The `xpost-test` target is `aarch64-unknown-linux-musl`.
On this Mac, its path toolchain delegates Cargo to Docker.
The default container `quirky_villani` must already be running.
The shim stages sources under `/Users/nan/.cache/xpost-test-docker` and copies build outputs back to `xpost-test/target/`.
The host build embeds the eBPF object.
`TEST_SKIP_EBPF` produces a stub and is unsuitable for TR3000 runs.
Use these manual steps only when necessary:

```bash
cd xpost-test && cargo build --release
scp -O target/aarch64-unknown-linux-musl/release/xpost-test tr3000:~/xpost-test
# On TR3000:
./xpost-test
```

Read stdout and stderr directly from the terminal.
Do not redirect test output to `/tmp` files.
Do not use `tee` or `tail` to process test logs.
For long runs, use the execution tool's session support to keep the process active.
Continue to read its terminal output.

### Experiment settings

The `run` list in `config/default.toml` selects the experiments and their order.
The supported entries are `just_tx`, `just_drop`, `firewall`, `exec_bench`, `dev`, and `formal-clear`.
The same file contains the `[postgres]`, `[agent]`, `[dut]`, and `[probe]` settings.

- Only `just_tx`: `run = ["just_tx"]`.
- Only `firewall`: `run = ["firewall"]`.
- Current selection: `run = ["exec_bench"]`.
- All network experiments: `run = ["just_tx", "just_drop", "firewall"]`.
- All formal experiments: `run = ["just_tx", "just_drop", "firewall", "exec_bench"]`.

The experiment files contain `formal` and `arms` settings.
The three network experiment files use `formal = true` and `arms = ["hal", "xdp"]`.
These network settings apply to the CAG and Trace stages below.
The CAG stage runs each grid point for 30 seconds per arm, in the configured arm order.
With the current configuration, each experiment then runs an `xdp_t` stage with one packet.
`just_tx` skips this Trace stage for HAL-only arms.
`just_drop` and `firewall` always append it.
The Trace stage stores `target_pps=1` and `window_s=0`.

All three network grids use 13 rates: 400–1600 pps in steps of 200, then 2000–4000 pps in steps of 400.
Each experiment defines its own `SWEEP_PPS` and `WINDOW_S` constants.
Legacy fields in `just_drop.toml`, including `fixed_pps`, `trace_pps`, `rounds`, and sample/instrumentation settings, are not consumed by the current config structs.
The `just_drop` experiment reads `get map counter` into `just_drop_data`.
The counter uses the last byte of the IPv4 source address as its bucket index.

The `firewall.toml` file sets `blacklist_size = 200` and `legal_packets = 1024`.
Each firewall CAG cell sends illegal traffic from blacklist source IPs at the selected rate for 30 seconds.
The same window includes exactly `legal_packets` unique little-endian `u16` tokens from a source IP outside the blacklist.
The DUT stores legal tokens in the virtual map `firewall_result` (`get map` / `clear map`).
Legal flows do not ACK.
`firewall_config.sent` stores the illegal send count.
`legal_sent` stores the legal send count. `matched` stores the matched token count.
Despite its name, `loss` stores the delivery ratio `matched / legal_sent`.
The firewall Trace stage uses eight blacklist entries and sends one matching packet.

The network XDP firmware projects are `examples/just_xdp` and `examples/firewall`.
Their board features are `perf-cag` and `perf-t`, with no default feature.
HAL firmware uses `examples/just_tx_hal`, `examples/just_drop_hal`, `examples/firewall_hal`, and `examples/exec_bench_hal`.
Those crates have no Cargo perf feature.
`hal_perf` always encodes Counter, Aggregate, Gauge, and Trace.
The `profiles.toml` registry contains eleven `[[fw]]` names:

- XDP: `just-tx-cag`, `just-tx-t`, `just-drop-cag`, `just-drop-t`, `firewall-cag`, and `firewall-t`.
- HAL: `just-tx-hal`, `just-drop-hal`, and `firewall-hal`.
- Exec: `exec-bench-ebpf` and `exec-bench-hal`.

Its network `[[xdp]]` names are `just_tx`, `just_drop`, and `firewall`.
Its exec `[[xdp]]` names are `incr`, `square`, `bitswap`, `fib`, `buf`, `memcpy`, `fletcher32`, and `bsort`.
The `exec-bench-ebpf` firmware profile installs `incr` by default.
The `exec-bench-hal` profile omits `xdp`, which defaults to an empty list.

### Exec experiment

`exec_bench.toml` uses `formal = true` and `arms = ["ebpf", "hal"]`.
The host accepts `xdp` as an alias for the `ebpf` arm.
The eight workloads and their 32-byte input/output fixtures reside in `xpost-test/common/src/exam/exec_bench.rs`.
The configuration does not select workloads or repetition counts.
Each arm runs each workload once. The collector stores `rep = 1`.
The task selection follows the eight benchmarks in the HAVM paper (CAV 2024, Section 7, Table 1).
Group them as arithmetic/logic (`incr`, `square`, `bitswap`, `fib`), memory access (`buf`, `memcpy`), and data processing (`fletcher32`, `bsort`).
State the local input sizes and platform. Shared task names do not establish an identical reproduction of HAVM's experiment.

The host performs these steps for each workload:

1. For the eBPF arm, clear the cold table with `clear xdp *`.
2. For that arm, install the workload XDP ELF and wait for `OK init`.
3. Clear the performance report.
4. Write the input to key `0` in map `workload`.
5. Send `enable exec`.
6. Wait for `OK finish`.
7. Attempt `disable exec`.
8. Read key `1` from map `workload`.
9. Compare all 32 output bytes with the expected fixture.
10. Store the successful condition and its performance report.

Clearing the cold table before each exec image places the image at offset zero for flash-sector alignment.
The host aborts on enable, finish, or output-check failure before inserting that condition.
Performance collection occurs after condition insertion. A later failure can leave partial data in an unfinished exam.
The collector currently ignores the result of `disable exec`.

The `ebpf` database label identifies the image-loading path.
eBPF-arm firmware compiles Counter, Aggregate, Gauge, and Trace into the report.
`exec_call::invoke` records `xdp_invocations` / `xdp_action` (Trace `xdp_begin` / `xdp_end` and the `execution` aggregate).
HAL `enable exec` records the same counter names plus `execution` ticks through `hal_perf`.
It also pushes Trace `xdp_begin`, printk helper enter/leave (id 6), and `xdp_end`.
The exam writes eBPF Trace into `trace_event` (`firmware` tag `xdp_t`).
It writes HAL CAGT into `exec_bench_hal_*` plus `trace_event`.
It aborts collection if Trace lacks `xdp_begin`, `xdp_end`, or the printk helper pair.
This check occurs after the condition and result rows enter the database.
`OK finish` is the host wait only.
The report's `runtime_us` is the invocation interval minus its nested printk intervals (helper id 6).
This value includes work outside the algorithm itself.
HAL Trace includes result-map insertion. XDP Trace excludes the carrier's result-map insertion.
The HAL `execution` aggregate measures `run_hal` only. The XDP aggregate includes invocation and printk work.
Do not treat these aggregate timers as equivalent.
Check Trace pairs, event order, nesting, and successful return before subtracting printk time.
Keep the full invocation and printk durations beside the derived value.
Do not report helper time or the host command interval as computation latency.
Do not report algorithm speedup ratios from these differently scoped intervals.

### HAL smoke test

For HAL smoke testing, set `run = ["dev"]`.
This mode flashes `just-tx-hal`, `just-drop-hal`, and `firewall-hal` in order.
It checks TX replies and `rxcnt`, DROP counters, and firewall blacklist hit counts plus `firewall_result` token capture.
TX and DROP use 100 pps for two seconds.
Firewall sends 32 hit probes and 32 miss token probes.
It runs no XDP arm, ignores perf reports, and writes no database records or Trace report file.

See the `xpost-test` rustdoc modules `overview` and `config` for the complete procedure.
See `xpost-serial` for the serial API.

### Database rules

Formal collection decodes perf reports into tables associated with an exam.
The tables include `exam`, experiment configuration and data tables, `xdp_*` and HAL perf tables, and `trace_event`.
The collector does not store `report_raw`.
The active schema files in `xpost-test/sql/` are `formal.sql`, `just_drop.sql`, `just_tx.sql`, `firewall.sql`, and `exec_bench.sql`.
`dev.sql` remains in the directory but is not loaded by the current collector.
HAL CAG tables are per experiment: `<experiment>_hal_counter`, `<experiment>_hal_agg`, and `<experiment>_hal_gauge`.
Exec stores inputs and results in `exec_bench_config` and `exec_bench_data`.
Its performance reports use shared `xdp_*` tables for the `ebpf` arm and `exec_bench_hal_*` tables for HAL.

Starting a run must not execute `DROP` or `TRUNCATE`.
For an explicit database clear, set `run = ["formal-clear"]`.
Rebuild the binary.
Upload it to TR3000.
Run it without arguments.
This operation executes `TRUNCATE exam CASCADE`.

## Runtime constraints

Use bare-metal firmware only, without ESP-IDF.
Use static loading to place XDP ELF image bytes into slots.
Do not parse the firmware ELF on the device.
Preserve the ABI and linker layout shared with libbpf-mcu through the XPM1 contract and the board's `partitions.csv`.

XDP firmware calls `xpost_build::firmware_codegen()` in `build.rs`.
It reads `[package.metadata.xpost]` defaults from the root crate, applies board overrides, and derives the cold Flash span from `partitions.csv`.
The `xpost-build/xpost-link` wrapper embeds the `.xpost.mem` XPM1 contract into the linked Firmware ELF for host tools.
The current XDP boards each override the layout to one primary slot `a`, with `mount = "rx"`.
The library defaults also define slot `b`.

### Maps and boot

The XDP firmware starts the runtimes in this order:

1. `MapRt::init` claims the talc heap.
2. `XdpRt::init` registers slots and scans the cold XDP ELF images.
3. Inside `XdpRt::init`, `create_required_maps` calls `MapRt::create` for missing `required_maps` and checks existing maps.
4. The board switches the first discovered image into the primary slot.
5. The board attempts to lease the image.
6. The board drops the runtime guards before RX or exec commands start.

Without an active lease, ingress passes packets to the network stack.
HAL firmware uses application arrays/hash maps and does not initialize `MapRt` or `XdpRt`.

When it leases an image, the runtime installs VMR-to-RMA aliases from `aot_binds` through `set_map_aliases`.
Map names reside in `MapRt` metadata and XDP ELF descriptors.
There is no userspace `register_map_name` path.

Control-plane errors use the small enums in `crate::error` for each public entry point.
BPF helper errno values use `map::ops::MapOpError`.
Keep these helper errors separate from control-plane enums.

### Performance and ingress

The crate mounts a private `xdp::ingress::XdpProg` that implements the `xpost_type::XdpProg` trait.
With the `xdp` feature, esp-radio also exports this trait as `esp_radio::wifi::XdpProg`.
Applications cannot access `XDP_PROG`.
They read performance data through four public accessors:

- `ext::perf::counter()`
- `ext::perf::aggregate()`
- `ext::perf::gauge()`
- `ext::perf::trace()`

The shared `report_perf` and `clear_perf` functions support the App commands `get perf` and `clear perf`.
The `EspRadioTrace` integration resides in `ext/perf/trace/`, beside the ring storage.

- `esp-radio`: `xpost::esp_radio::new` calls `wifi::new(device, config, &ingress::XDP_PROG)`.
- `perf-counter`: Counter storage and `EspRadioTrace` hooks on the mounted program.
- `perf-aggregate`: One stretch histogram per pipeline span and per dispatched helper.
  XDP records helper time in those per-helper histograms.
  HAL `map_operation` remains its own aggregate.
  The helper dispatcher does not fill `map_operation`.
- `perf-gauge`: Current and peak values.
- `perf-trace`: Event rings grouped by category.

Trace provides the only timeline.
Each event has a timestamp.
Nested intervals use paired `Begin` and `End` events on the same ring.
The application-category ring slots remain reserved and empty.
Current HAL firmware encodes CAGT. The exec HAL firmware writes events to the radio-category ring.
The pinned network batches predate HAL Trace collection and contain no HAL timeline.
Formal runs at high packet rates use Counter, Aggregate, and Gauge data instead of continuous Trace collection.

The `xpost-type` crate defines the shared traits for xpost and esp-radio: `EspRadioTrace: XdpProg`.
The esp-radio crate controls RX, maps `XdpAction` to `Action`, and calls the `EspRadioTrace` hooks.
The mounted `XdpProg::run` delegates lease checks, image invocation, and packet-window validation to `xdp::ingress::run`.
UDP application logic remains in the firmware crate.
With `exec-call`, `ext::exec_call::invoke(&mut [u8])` calls the same lease-controlled `xdp::ingress::run` function.
This interface does not attach another network ingress hook.
Without an active lease, it returns `XdpAction::Pass` and the full buffer.
The image uses the caller's stack. There is no dedicated XDP stack switch.
HAL CAGT values use the same wire layout but measure application-level intervals.
Compare measurement boundaries before comparing HAL and XDP metrics with the same name.

The optional `ext::ringbuf` module is incomplete.
Enabling the Cargo feature `ringbuf` triggers `compile_error!`.

## Benchmark analysis

Read `benchmark/README.md` before analysis work.
`benchmark/report.md` presents ARRAY, HASH, TX, then EXEC.
Read `xpost-test/common/src/exam/{just_drop,firewall,just_tx,exec_bench}.rs` before interpreting results.
Check the current firmware and build scripts too. Comments and older design drafts can describe unimplemented behavior.
`benchmark/analysis/` generates figures and evidence tables from a fixed database snapshot. It does not rewrite the report prose or generate notebooks.
`benchmark/report.toml` pins one completed exam per feature and the expected experiment grid.
From `benchmark/`, run `pixi run figures` for a read-only database export and figure build, or `pixi run replay --snapshot outputs/snapshots/<id>` to rebuild offline.
Figures, CSV tables, and provenance go under `benchmark/outputs/report/`. Review the Markdown narrative when changing batches.
TX reports reply throughput, end-to-end loss, and RTT.
HASH distinguishes intended policy drops from missing legal tokens.
Its legal-flow delivery ratio uses token-set matching through `firewall_result`, without ACKs.
EXEC reports output checks and available timing samples. Missing execution samples do not mean zero execution time.
`[exec_bench].require_trace = true` in `benchmark/report.toml` requires complete Trace timing for all sixteen EXEC conditions.
Older snapshots can omit this setting and retain missing timing values.
`benchmark/analysis/exec_bench.py` checks the EXEC conditions independently from the network rate grids.
The export preserves its 32-byte inputs and outputs as hex strings.
Older snapshots without EXEC remain valid for the three network experiments.
Timed-region duration is not whole-device CPU utilization. Older snapshots may lack HAL Trace.
Keep zero-sample means missing, distinguish target from actual send rate, and do not compare differently scoped HAL/XDP timers as speedup ratios.
Run `pixi run check` for analysis checks.
Regenerating the report requires no device operation.
Use `benchmark/pixi.toml` for the Python environment and `benchmark/utils/db.py` for database access.
The database helper reads `benchmark/.env`.
Set `PGSCHEMA=formal` in that file for formal results.
The standalone `export_flame_svg.py` and `export_speedscope.py` retain legacy report/schema inputs.
They are not the current formal collector's export path.
Keep analysis organized by feature or metric, as specified in the benchmark README.
Do not restore the deleted per-experiment notebooks without an explicit request.

## Documentation style

Use `.agents/skills/asd-ste100/SKILL.md` for English agent instructions.
Use `.agents/skills/ste-zh/SKILL.md` for Chinese technical text.
Use one action per instruction and one term per concept.
Preserve conditions, uncertainty, numbers, units, and machine-readable identifiers.
Split compound instructions into separate sentences. Do not join actions with semicolons.
Keep English instructions within 20 words and descriptions within 25 words where precision permits.
Keep Chinese instructions within 25 characters and descriptions within 40 characters where precision permits.
Delete vague wording, empty phrases, and marketing claims.
Run the corresponding `scripts/ste-lint.py` or `scripts/ste-lint-zh.py` with Python.
On hosts without `python`, use `python3`.
Review findings manually. Preserve event names and code identifiers when the linter mistakes them for prose.

For the report, explain each experiment before interpreting its results.
Use pipeline pseudocode for experimental steps.
Define network and MCU terms before use.
Write for readers with computer knowledge but no MCU or networking background.
Keep the narrative focused on measured results and their limits.
Exclude internal reasoning, intermediate work notes, and prompt-like instructions.
Use readable legends that identify the compared paths and quantities.
Do not create speedup plots without comparable timing boundaries and nonzero sample counts.

## Archive protection

The `benchmark/backup/` directory contains frozen archives.
Do not change or delete these files without an explicit user request.
