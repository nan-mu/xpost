# xpost

Rust MCU XDP runtime (crate `xpost`) plus board examples and host tooling.
Phase 1: ESP32-C3 / RV32IMC.

## Artifact names (do not confuse)

| Name | What it is | Typical path |
|------|------------|--------------|
| **Firmware ELF** | Board app (`XdpRt`, Wi-Fi, …) | `examples/<app>/…/release/<app>` |
| **XDP ELF** (`xdp.elf`) | Cold-region XDP container (`.xpost.meta` + `.xpost.image`) | `../build/<stem>/<stem>.elf` |
| **`esp32c3.elf`** | libbpf-mcu AOT debug ELF (not for cold install) | `../build/<stem>/esp32c3.elf` |

Prefer the term **XDP ELF** / **`xdp.elf`** in docs and logs (not “Compact ELF”).

## Layout

- Root crate `xpost`: board-agnostic runtime (maps, helpers, IRAM slots, RX ingest).
- `examples/<app>/`: self-contained MCU projects.
- Host (workspace `exclude`, own Cargo workspace / triple): `xpost-serial`
  (library), `xpost-agent` (CLI), `xpost-build`, `xtask`, `xpost-test`
  (experiment harness; same git tree, separate `Cargo.toml`).

## Host serial / agent / test

- **`xpost-test/`**: **owns the full test workflow** (formal `just_drop` /
  `just_tx`, `dev`, `formal-clear` via `config/default.toml` `run`). Sole
  binary entry: bare `./xpost-test` (no CLI flags/subcommands). See rustdoc
  modules `overview` + `config`. Calls agent HTTP; does not hold the USB
  session while the agent is running.
- **`xpost-agent/`**: CLI + `--listen` HTTP `/v1/*` and MCP `/mcp` on one port
  (shared USB). Dispatch: `cargo xtask agent …`.
  Map read: DUT `map dump <name>` (multi-line → `OK map end`); HTTP
  `GET /v1/map/{name}` / MCP `map_dump`. Do not use legacy `rxcnt <key>`.
- **`xpost-serial`**: library only — wire [`type`](xpost-serial/src/type/) plus
  feature `client` (`Client` = serial + flash threads, unified `Command` incl.
  `Flash`) or mutually exclusive `server` (DUT `dispatch` / reply builders).
  App UART: `ping` / `enable|disable xdp` / `clear|get|update map` /
  `clear|get perf` (blob). Cold install **by XDP name** via flash thread.
  Perf metrics live in `xpost::ext::perf`; host decode is
  `client::serial::decode_perf`. Formal decodes the report and writes exam-scoped tables
  (`sql/formal.sql`: `exam` + `just_*_config/data` + `xdp_*`/`hal_*` +
  `trace_event`) — no `report_raw`. Open-run never DROP/TRUNCATE; explicit
  wipe is `run = ["formal-clear"]` in `config/default.toml` then rebuild
  (`TRUNCATE exam CASCADE`). Trace is the
  **sole timeline**: categorized rings of timestamped events (nest =
  paired Begin/End on the same ring). Flame export is **XDP-only** split
  path charts (`export_flame_svg`: `just_tx-xdp-*-path-rx|path-tx.flame.*`;
  x = ``t_us`` within episode). Prefer short `dev` capture →
  `/tmp/xpost-dev-flame.report` → Mac `target/dev-flame.report`. High-pps
  formal uses Counter/Aggregate/Gauge (no Trace flood). HAL app-category
  ring slots are reserved empty.
- **`--port`**: optional. Omit → unique USB serial; zero or many → require
  `--port`.

### Experiment module rustdoc (`xpost-test/test/src/bench/`)

Structure per formal experiment: **目的 → 伪代码 → 发包/dataplane 一段 → 数据处理**
（`config` / `data` 两个无序列表）。记号约定以后收到上级模块，勿在各实验重复长段。

Writing the dataplane paragraph:

- State facts only; do not introduce or define jargon (`stim`, …). Prefer plain
  verbs（发往、读出、+1、DROP）.
- Only mention settings that matter to the measurement. Skip what the
  pseudocode already covers (e.g. pps grid) and incidental fields（目的 IP —
  「发往 DUT」已含）.
- Prefer dense prose over bullet inventories of headers/ports/payloads.
  Fold constants into the sentence（「DUT 的 `STIM_PORT`」「载荷为 1 字节」）.
- Keep one consistent pair of terms for address roles（源 / 目的，或 src / dst）；
  do not mix styles in the same paragraph.

## Commands

```bash
cargo xtask just_drop build --release

# Install / overwrite just_drop XDP ELF in the cold table (default just_drop.elf)
cargo xtask agent program --elf examples/just_drop/just_drop.elf
# or omit --elf (same default)

# List cold-table entries
cargo xtask agent list

# Erase all XDP ELF containers in the cold table
cargo xtask agent clear

cargo xtask agent request PING
cargo xtask agent request counters
cargo xtask agent probe <esp-ipv4>

# Long-running RPC for xpost-test / fwctl (default device session)
cargo xtask agent --listen 0.0.0.0:9240 --no-monitor
```

`probe` sends one UDP datagram to port 8080 and prints `ack` or `no-ack`.
Drop XDP should yield `no-ack` and a rising `drop` counter. `counters` is one
`OK counters …` line (`drop`, `failed_tx`, `net_rx`, and the runtime error
counters). `ack-dest <ipv4> <port>` points the firmware ACK at a fixed listener;
otherwise the ACK returns to the stimulus source.

On TR3000, `./probe just-drop` runs the ACK functional path (50 stims, or 10 with
`--smoke`) then a UDP×4 link-rate blast (`--no-perf` skips perf;
`--window-secs` overrides 40s / smoke 10s). Use `--no-pg` to skip Postgres.

Wi-Fi STA needs `WIFI_SSID` and `WIFI_PASS` (or `WIFI_PASSWORD` in
`examples/just_drop/.env`, loaded by that crate's `build.rs`). Without them the
UDP path stays down and serial `counters` still works.

## Control: `just_drop_hal`

crates.io-only esp-hal / embassy-net / smoltcp firmware — **no** path deps into
the xpost tree, **no** local `esp-radio` fork, **no** cold XDP / `agent program`.

```bash
cargo xtask just_drop_hal build --release
cargo xtask agent request "drop off"   # Pass: ACK + net_rx↑
cargo xtask agent request "drop on"    # Drop: no-ack + drop/rxcnt[17]↑
cargo xtask agent probe <esp-ipv4>
cargo xtask agent request counters
cargo xtask agent request "rxcnt 17"
```

Drop is application-layer on UDP:8080 (packet already entered smoltcp), not
Wi-Fi ingress. Serial surface matches `just_drop` (`ping` / `counters` /
`rxcnt` / `ack-dest`); XDP error fields in `OK counters` are always 0,
`map_lock=no-rt`. Wi-Fi creds: `examples/just_drop_hal/.env` (same keys).

### PPS A/B (`benchmark/`)

Pixi + Jupyter；数据经 `utils.db` 现查 Postgres（`benchmark/.env`）：

```bash
cd benchmark && pixi install && pixi run ensure-schema && pixi run lab
# notebook: just_drop_pps.ipynb（内含复现流程）
# TR3000 采集：
./probe just-drop-pps --path hal|xdp --out /tmp/….csv
# 入库（显式 CSV + summary，写入 experiment_*）：
pixi run ingest-pps -- /tmp/….csv /tmp/…_summary.json
```

See [`benchmark/README.md`](benchmark/README.md).

## Scope

Bare-metal only (no ESP-IDF). Static loading of XDP ELF image bytes into slots;
no on-device ELF parse of the firmware image. Preserve ABI / linker layout with
`libbpf-mcu` XPM1 and board `partitions.csv`.

### Maps / boot

Firmware-first: `MapRt::init` claims the talc heap, then `XdpRt::init` scans cold
XDP ELF and `MapRt::create`s each image's `required_maps`. Lease installs VMR→RMA
aliases from meta `aot_binds` (`set_map_aliases`). There is no userspace
`register_map_name` path — map names live in `MapRt` meta and XDP ELF descriptors.

Control-plane errors are `crate::error` (small enums per public entrypoint).
BPF helper errno is `map::ops::MapOpError` (not nested into control-plane enums).

### Trace (`ext::trace` / ingress program)

The crate mounts a private `xdp::ingress::XdpProg` implementing `xpost_type::XdpProg`
(re-exported as `esp_radio::wifi::XdpProg` under the `xdp` feature). Apps do **not**
see `XDP_PROG`; they read via `ext::trace::counter()` / `timestamp()`.

- `esp-radio` — `xpost::esp_radio::new` → `wifi::new(device, config, &ingress::XDP_PROG)`
- `trace-counter` — `ext::trace::counter::Counter` + `Observe` on the mounted prog
- `trace-timestamp` — helper enter/leave ring on the same prog

Traits live in the `xpost-type` crate so esp-radio (xdp feature) and xpost share
one declaration (`Observe: XdpProg`). esp-radio owns RX / `XdpAction`→`Action`
mapping / Observe call-sites; `XdpProg::run` only invokes the leased XDP image.
App fields like `net_rx` stay in the firmware crate.

Optional / incomplete: `ext::ringbuf` behind Cargo feature `ringbuf` (currently
`compile_error!`).