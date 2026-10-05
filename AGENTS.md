# xpost

The `xpost` crate provides an XDP runtime for Rust MCU applications.
This repository also contains board examples and host tools.
The Phase 1 target is ESP32-C3 with RV32IMC.

## Artifact names

Use these names in documents and logs:

- **Firmware ELF**: The board application, including Wi-Fi and, for XDP firmware, `XdpRt`.
  HAL firmware has no xpost runtime.
  Typical path: `examples/<app>/.../release/<app>`.
- **XDP ELF** (`xdp.elf`): The cold-region XDP container with `.xpost.meta` and `.xpost.image` sections.
  Typical path: `../build/<stem>/<stem>.elf`.
- **`esp32c3.elf`**: The libbpf-mcu AOT debug ELF.
  Typical path: `../build/<stem>/esp32c3.elf`.
  Do not use this file for cold installation.

Use **XDP ELF** or **`xdp.elf`** instead of "Compact ELF" in documents and logs.

## Repository layout

- The root crate provides the shared runtime: maps, helpers, IRAM slots, and RX input processing.
- `esp-radio` is the local radio driver with XDP ingress and performance hooks.
- `xpost-type` provides the shared XDP actions and traits.
- `xpost-elf` provides XDP ELF encoding/decoding and the XPM1 memory-map contract.
- `xpost-macros` provides map derives and the `flash_layout!` wire-layout macro.
- The firmware applications under `examples/` have separate Cargo workspaces.
  `examples/hal_perf` is a shared library for HAL CAG reports and map wire helpers.
  `examples/esp32c3` is a Wi-Fi starter project, outside the formal profile registry.
- `xpost-serial` provides the serial protocol library.
- `xpost-agent` provides the host daemon.
- `xpost-build` provides build tools.
- `xpost-test` controls the complete test procedure.
- `benchmark` contains Python analysis tools and a feature-oriented Markdown experiment report.

The root workspace members are `.`, `esp-radio`, `xpost-macros`, `xpost-elf`, and `xpost-type`.
It excludes `xpost-build`, `xpost-agent`, `xpost-serial`, `xpost-test`, and `examples`.
The four excluded packages have separate Cargo workspaces.
The root defaults to `riscv32imc-unknown-none-elf`; `xpost-agent` and `xpost-serial` override it with `aarch64-apple-darwin`.
`xpost-build` has no target override; use an explicit host `--target` when building or testing it directly.
`xpost-trait/` contains older traits and is not referenced by the current workspace members or dependencies; use `xpost-type`.
The `xpost-test` workspace contains `common` and `ebpf`.
The `common` package provides the ABI library and the `xpost-test` binary.
The `ebpf` package uses aya: TC egress timestamps and XDP ingress timestamps, with TC ingress as an attach fallback.
The host modules are `agent`, `config`, `db`, `pipeline`, and `exam`.

## Agent access

The `xpost-agent` daemon controls the shared USB connection.
It serves HTTP `/health` and `/v1/*`, and Streamable HTTP MCP `/mcp`, on one port.
Startup opens the USB session only; App and flash operations run when a request arrives.

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
The build runs in `examples/<app>` with `cargo build --release --features ...`.
For profiles with `fw.xdp[]`, it then clears the cold table and builds and installs the linked XDP images.
HAL profiles have `xdp = []` and skip this step.
XDP profile builds run `pixi run build --firmware ... -c <name>` in the sibling `../libbpf-mcu` repository.

Each firmware project reads `WIFI_SSID` and `WIFI_PASS` from its own `.env` file:

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
There are no default features; `client` and `server` are mutually exclusive unless `dev` is enabled.
The in-memory protocol tests run from `xpost-serial/` with `cargo test --features "dev,client,server"`.

The App wire types are `xpost_serial::command::Command` and `xpost_serial::reply::OkReply`.
The App commands are:

- `ping`
- `enable xdp` and `disable xdp`
- `clear map`, `get map`, and `update map`
- `clear perf` and `get perf`

A successful `get map` or `get perf` response contains an `OK get ... <n>` header and `n` blob bytes.
The map blob is an ASCII snapshot, generated by `xpost::map::dump` for XDP firmware and `hal_perf::wire` for HAL firmware.
The perf blob uses the `ext::perf` layout.
HAL reports use the compatible CAG layout from `examples/hal_perf`.
`Command::GetInit` waits for unsolicited `OK init` on the host; it is not a UART command.
Cold installation selects an image by its XDP name.

## Test procedure

**Use a formal run by default when the user asks to test, run, or verify the system.**
On TR3000, always run the binary as `./xpost-test`.
Do not add environment variables, subcommands, or CLI flags to a formal run.

The build embeds these files from `xpost-test/config/`:

- `default.toml`
- `just_tx.toml`
- `just_drop.toml`
- `firewall.toml`

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
On this Mac, its path toolchain delegates Cargo to Docker; the default container `quirky_villani` must already be running.
The shim stages sources under `/Users/nan/.cache/xpost-test-docker` and copies build outputs back to `xpost-test/target/`.
The host build embeds the eBPF object; `TEST_SKIP_EBPF` produces a stub and is unsuitable for TR3000 runs.
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
The supported entries are `just_tx`, `just_drop`, `firewall`, `dev`, and `formal-clear`.
The same file contains the `[postgres]`, `[agent]`, `[dut]`, and `[probe]` settings.

- Only `just_tx`: `run = ["just_tx"]`.
- Only `firewall`: `run = ["firewall"]`.
- All formal experiments (current order): `run = ["just_tx", "just_drop", "firewall"]`.

The experiment files contain `formal` and `arms` settings.
All three current experiment files use `formal = true` and `arms = ["hal", "xdp"]`.
The CAG stage runs each grid point for 30 seconds per arm, in the configured arm order.
With the current configuration, each experiment then runs an `xdp_t` stage with one packet.
`just_tx` skips this Trace stage for HAL-only arms; `just_drop` and `firewall` always append it.
The Trace stage stores `target_pps=1` and `window_s=0`.

All three formal grids use 400–1600 pps in steps of 200, then 2000–4000 pps in steps of 400 (13 rates).
Each experiment defines its own `SWEEP_PPS` and `WINDOW_S` constants.
Legacy fields in `just_drop.toml`, including `fixed_pps`, `trace_pps`, `rounds`, and sample/instrumentation settings, are not consumed by the current config structs.
The `just_drop` experiment reads `get map counter` into `just_drop_data`.
The counter uses the last byte of the IPv4 source address as its bucket index.

The `firewall.toml` file sets `blacklist_size = 200` and `legal_packets = 1024`.
Each formal CAG cell mixes illegal traffic (blacklist source IP at the pps grid for 30s) with exactly `legal_packets` unique little-endian `u16` tokens on a miss source IP.
Legal tokens are stored on the DUT and read back through the virtual map name `firewall_result` (`get map` / `clear map`); legal flows do not ACK.
`firewall_config.sent` stores the illegal send count; `legal_sent`, `matched`, and `loss` (`matched / legal_sent`) store the legal-flow delivery ratio.
The firewall Trace stage uses eight blacklist entries and sends one matching packet.

The XDP firmware projects are `examples/just_xdp` and `examples/firewall`.
Their board features are `perf-cag` and `perf-t`, with no default feature.
HAL firmware uses `examples/just_tx_hal`, `examples/just_drop_hal`, and `examples/firewall_hal`, with `perf-cag` and the shared `hal_perf` library.
The `profiles.toml` registry has nine `[[fw]]` names:

- XDP: `just-tx-cag`, `just-tx-t`, `just-drop-cag`, `just-drop-t`, `firewall-cag`, and `firewall-t`.
- HAL: `just-tx-hal`, `just-drop-hal`, and `firewall-hal`.

Its `[[xdp]]` names are `just_tx`, `just_drop`, and `firewall`.

For HAL smoke testing, set `run = ["dev"]`.
This mode flashes `just-tx-hal`, `just-drop-hal`, and `firewall-hal` in order.
It checks TX replies and `rxcnt`, DROP counters, and firewall blacklist hit counts plus `firewall_result` token capture.
TX and DROP use 100 pps for two seconds; firewall sends 32 hit probes and 32 miss token probes.
It runs no XDP arm, ignores perf reports, and writes no database records or Trace report file.

See the `xpost-test` rustdoc modules `overview` and `config` for the complete procedure.
See `xpost-serial` for the serial API.

### Database rules

Formal collection decodes perf reports into tables associated with an exam.
The tables include `exam`, experiment configuration and data tables, `xdp_*` and HAL perf tables, and `trace_event`.
The collector does not store `report_raw`.
The active schema files in `xpost-test/sql/` are `formal.sql`, `just_drop.sql`, `just_tx.sql`, and `firewall.sql`.
`dev.sql` remains in the directory but is not loaded by the current collector.
HAL CAG tables are per experiment: `<experiment>_hal_counter`, `<experiment>_hal_agg`, and `<experiment>_hal_gauge`.

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
The current XDP boards each override the layout to one primary RX slot `a`; the library defaults also define slot `b`.

### Maps and boot

The XDP firmware starts the runtimes in this order:

1. `MapRt::init` claims the talc heap.
2. `XdpRt::init` registers slots and scans the cold XDP ELF images.
3. Inside `XdpRt::init`, `create_required_maps` calls `MapRt::create` for missing `required_maps` and validates existing maps.
4. The board switches the first discovered image into the primary slot and attempts to lease it, then drops the runtime guards before RX starts.

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
- `perf-aggregate`: Latency aggregates and a stretch histogram for high packet rates.
- `perf-gauge`: Current and peak values.
- `perf-trace`: Event rings grouped by category.

Trace provides the only timeline.
Each event has a timestamp.
Nested intervals use paired Begin and End events on the same ring.
The application-category ring slots remain reserved and empty; current HAL firmware reports CAG only.
Formal runs at high packet rates use Counter, Aggregate, and Gauge data instead of continuous Trace collection.

The `xpost-type` crate defines the shared traits for xpost and esp-radio: `EspRadioTrace: XdpProg`.
The esp-radio crate controls RX, maps `XdpAction` to `Action`, and calls the `EspRadioTrace` hooks.
The mounted `XdpProg::run` delegates lease checks, image invocation, and packet-window validation to `xdp::ingress::run`.
UDP application logic remains in the firmware crate.
HAL CAG values use the same wire layout but measure application-level intervals.
Compare measurement boundaries before comparing HAL and XDP metrics with the same name.

The optional `ext::ringbuf` module is incomplete.
Enabling the Cargo feature `ringbuf` triggers `compile_error!`.

## Benchmark analysis

Read `benchmark/README.md` before analysis work.
`benchmark/report.md` presents the experiment design and results in ARRAY, HASH, and TX order.
Read `xpost-test/common/src/exam/{just_drop,firewall,just_tx}.rs` to explain the workload and measurement semantics before interpreting its metrics.
`benchmark/analysis/` generates figures and evidence tables from a fixed database snapshot. It does not rewrite the report prose or generate notebooks.
`benchmark/report.toml` pins one completed exam per feature and the expected experiment grid.
From `benchmark/`, run `pixi run figures` for a read-only database export and figure build, or `pixi run replay --snapshot outputs/snapshots/<id>` to rebuild offline.
Figures, CSV tables, and provenance go under `benchmark/outputs/report/`. Review the Markdown narrative when changing batches.
TX reports reply throughput, end-to-end loss, and RTT. HASH distinguishes intended policy drops from loss; the current collector has no matched ACK series for end-to-end firewall loss.
Timed-region duration is not whole-device CPU utilization. HAL has no Trace timeline in the current formal batches.
Keep zero-sample means missing, distinguish target from actual send rate, and do not compare differently scoped HAL/XDP timers as speedup ratios.
Run `pixi run check` for analysis checks; no device operation is needed to regenerate the report.
Use `benchmark/pixi.toml` for the Python environment and `benchmark/utils/db.py` for database access.
The database helper reads `benchmark/.env`; use `PGSCHEMA=formal` for formal results.
The standalone `export_flame_svg.py` and `export_speedscope.py` retain legacy report/schema inputs; they are not the current formal collector's export path.
Keep analysis organized by feature or metric, as specified in the benchmark README.
Do not restore the removed per-experiment notebooks without an explicit request.

改写中文技术文本时套用受控中文规则：一句一动作、主动语态、一词一义（同一概念全篇同一个词）、 不用分号连接动作、句长（指令 ≤ 25 字、说明 ≤ 40 字）、去掉虚动词（进行 / 加以 + 名词）、 套话（需要注意的是、在一定程度上）、营销形容词（无缝、强大、赋能、闭环）与含糊词（若干、一些、相关）； 不得改动情态（可能 / 也许）：置信度是内容。机械首查用 python .agents/skills/ste-zh/scripts/ste-lint-zh.py <文件>。

## Archive protection

The `benchmark/backup/` directory contains frozen archives.
Do not change or delete these files without an explicit user request.
