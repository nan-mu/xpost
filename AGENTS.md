# xpost

The `xpost` crate provides an XDP runtime for Rust MCU applications.
This repository also contains board examples and host tools.
The Phase 1 target is ESP32-C3 with RV32IMC.

## Artifact names

Use these names in documents and logs:

- **Firmware ELF**: The board application, including `XdpRt` and Wi-Fi.
  Typical path: `examples/<app>/.../release/<app>`.
- **XDP ELF** (`xdp.elf`): The cold-region XDP container with `.xpost.meta` and `.xpost.image` sections.
  Typical path: `../build/<stem>/<stem>.elf`.
- **`esp32c3.elf`**: The libbpf-mcu AOT debug ELF.
  Typical path: `../build/<stem>/esp32c3.elf`.
  Do not use this file for cold installation.

Use **XDP ELF** or **`xdp.elf`** instead of "Compact ELF" in documents and logs.

## Repository layout

- The root crate provides the shared runtime: maps, helpers, IRAM slots, and RX input processing.
- Each `examples/<app>/` directory contains a separate MCU project.
- `xpost-serial` provides the serial protocol library.
- `xpost-agent` provides the host daemon.
- `xpost-build` provides build tools.
- `xpost-test` controls the complete test procedure.

The root workspace excludes the four host packages.
These packages have separate Cargo workspaces and target settings.
The `xpost-test` workspace contains `common` and `ebpf`.
The `common` package provides the ABI library and the `xpost-test` binary.
The `ebpf` package uses aya TC.
The host modules are `agent`, `db`, `pipeline`, and `exam`.

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

Each firmware project reads `WIFI_SSID` and `WIFI_PASS` from its own `.env` file:

- `examples/just_xdp/`
- `examples/firewall/`
- `examples/just_drop_hal/`
- `examples/just_tx_hal/`
- `examples/firewall_hal/`

## Serial protocol

The `xpost-serial` crate is a library only.
Its shared `r#type` module defines the wire protocol.
Host tools enable the `client` feature.
Firmware enables the `server` feature, which provides `SerialHandler` and `serial_task!`.
Examples use `default-features = false, features = ["server"]`.

The App wire types are `xpost_serial::command::Command` and `xpost_serial::reply::OkReply`.
The App commands are:

- `ping`
- `enable xdp` and `disable xdp`
- `clear map`, `get map`, and `update map`
- `clear perf` and `get perf`

A successful `get map` or `get perf` response contains an `OK get ... <n>` header and `n` blob bytes.
The map blob is an ASCII snapshot from `xpost::map::dump`.
The perf blob uses the `ext::perf` layout.
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

To change an experiment, edit these TOML files.
Rebuild the binary.
Upload it before the next run.

Use the repository fish script from the repository root:

```bash
./xpost-test/scripts/xpost-test.fish
```

The script builds the binary, uploads it to TR3000, and runs it without arguments through SSH.
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
- All formal experiments: `run = ["just_drop", "just_tx", "firewall"]`.

The experiment files contain `formal` and `arms` settings.
The current configuration uses `arms = ["xdp"]`, with no HAL comparison.
The CAG stage runs each grid point for 30 seconds.
The pipeline then runs an `xdp_t` stage with one packet.
The Trace stage stores `target_pps=1` and `window_s=0`.

The formal `just_tx` and `just_drop` grids run from 400 to 4000 pps in steps of 400.
The `just_drop` experiment reads `get map counter` into `just_drop_data`.
The counter uses the last byte of the IPv4 source address as its bucket index.

The `firewall.toml` file also sets the blacklist `sizes` and the `traffic` modes, `hit` and `miss`.
The firewall Trace stage uses eight blacklist entries and sends one matching packet.

The XDP firmware projects are `examples/just_xdp` and `examples/firewall`.
Their board features are `perf-cag` and `perf-t`, with no default feature.
The `profiles.toml` registry contains no HAL profiles.
Its `[[fw]]` names are `just-tx-cag`, `just-tx-t`, `just-drop-cag`, `just-drop-t`, `firewall-cag`, and `firewall-t`.
Its `[[xdp]]` names are `just_tx`, `just_drop`, and `firewall`.

For debug Trace collection, set `run = ["dev"]`.
This mode uses `just-tx-t` at 100 pps for two seconds, for one round.
It writes no database records.
It writes the report to `/tmp/xpost-dev-flame.report`.

See the `xpost-test` rustdoc modules `overview` and `config` for the complete procedure.
See `xpost-serial` for the serial API.

### Database rules

Formal collection decodes perf reports into tables associated with an exam.
The tables include `exam`, experiment configuration and data tables, `xdp_*` and HAL perf tables, and `trace_event`.
The collector does not store `report_raw`.
The schema files in `xpost-test/sql/` are `formal.sql`, `just_drop.sql`, `just_tx.sql`, and `firewall.sql`.

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
Preserve the ABI and linker layout defined by libbpf-mcu XPM1 and the board's `partitions.csv`.

### Maps and boot

The firmware starts the runtimes in this order:

1. `MapRt::init` claims the talc heap.
2. `XdpRt::init` scans the cold XDP ELF images.
3. `MapRt::create` creates the maps listed in each image's `required_maps`.

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
The HAL application-category ring slots remain reserved and empty.
Formal runs at high packet rates use Counter, Aggregate, and Gauge data instead of continuous Trace collection.

The `xpost-type` crate defines the shared traits for xpost and esp-radio: `EspRadioTrace: XdpProg`.
The esp-radio crate controls RX, maps `XdpAction` to `Action`, and calls the `EspRadioTrace` hooks.
The `XdpProg::run` method only calls the leased XDP image.
Application fields such as `net_rx` remain in the firmware crate.

The optional `ext::ringbuf` module is incomplete.
Enabling the Cargo feature `ringbuf` triggers `compile_error!`.

## Archive protection

The `benchmark/backup/` directory contains frozen archives.
Do not change or delete these files without an explicit user request.
