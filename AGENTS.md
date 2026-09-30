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
  `just_tx`, `dev`, suite). See rustdoc modules `overview` + `config`. Calls
  agent HTTP; does not hold the USB session while the agent is running.
- **`xpost-agent/`**: CLI + `--listen` HTTP `/v1/*` and MCP `/mcp` on one port
  (shared USB). Dispatch: `cargo xtask agent …`.
- **`xpost-serial`**: library only — **API** for `Session` / `install_xdp` /
  `program` / `request` / `request_report` / `Sink` (see that crate’s rustdoc).
  Cold install is **by XDP name**; do not pass raw cold addresses on the normal
  path. Perf metrics live in `xpost::ext::perf`; DUT serial read face is
  `report` (binary, length → feature mask) + `perf clear`. Formal A/G/T
  stores the agent reply as `just_*_run.report_raw` (`OK report <n> b64=…`)
  without host-side decode into agg/gauge/trace child tables. Trace flame
  export is **XDP-only** split path charts (`export_flame_svg`:
  `just_tx-xdp-*-path-rx|path-tx.flame.*`; x = ``t_us`` within episode).
  Prefer short `dev` capture → `/tmp/xpost-dev-flame.report` → Mac
  `target/dev-flame.report`. HAL app echo is async and not role-compared with XDP_TX.
- **`--port`**: optional. Omit → unique USB serial; zero or many → require
  `--port`.

## Commands

Mac（USB 接 ESP；cwd = `xpost/`）：只需常驻 agent。收到 `FlashProfile` 时按 `profiles.toml` 的 `build` 编译再烧录。

HTTP：`/health`、`/v1/*`（harness）、`/mcp`（Cursor Streamable HTTP MCP）。

```bash
cargo xtask agent --listen 0.0.0.0:9240 --no-monitor
```

Cursor MCP 示例 URL：`http://127.0.0.1:9240/mcp`。
Wi-Fi 凭据仍在 `examples/just_drop/.env`、`examples/just_drop_hal/.env` 与 `examples/just_tx_hal/.env`（`WIFI_SSID` / `WIFI_PASS`）。

TR3000（编译期嵌入 `xpost-test/config/`：`default.toml` + `just_tx.toml` + `just_drop.toml`）：

**正式性能测试永远只跑无参二进制**——不要环境变量、不要子命令、不要 `--formal` / `--path`。改实验：改 TOML → 重编 → scp → `./xpost-test`。

```bash
cd xpost-test && cargo build -p test --release
scp -O target/aarch64-unknown-linux-musl/release/xpost-test tr3000:~/xpost-test
# on TR3000:
./xpost-test
```

- `config/default.toml`：`run = [...]`（跑哪些**实验**）、`[postgres]`、`[agent]`、`[dut]`、`[probe]`、版本与 profiles
- `config/just_tx.toml`：正式 just_tx（`arms` / `kinds=c|a|g|t` / 扫速+OP / `fixed_pps` / `trace_pps`）
- `config/just_drop.toml`：正式 just_drop（`arms` / `kinds=c|a|g|t` / `fixed_pps` / `trace_pps`）
- 调试火焰图 Trace：`run = ["dev"]`（硬编码：XDP × `just-tx-t` × 100 pps × 2s × 1
  轮；**无 DB**；report → `/tmp/xpost-dev-flame.report`；Mac scp 到
  `target/dev-flame.report` 后 `pixi run python -m utils.export_flame_svg`）
- Agent HTTP：`/health`、`/v1/*`（含 `/v1/serial_window`）、`/mcp`
- 实验步骤与全流程：`xpost-test` 的 `overview` / `config` rustdoc；串口 API：`xpost-serial`
- 正式 `just_tx` 扫速网格：`400…4000` pps（步长 400；ceiling ≈ XDP ~50% 丢包工作点的 hping3 标定）

只跑正式 just_tx：`run = ["just_tx"]`。一次跑全部 formal A/B：`run = ["just_drop", "just_tx"]`。

可选手工 XDP 冷表操作：`cargo xtask agent program|list|clear|request|probe …`。

### PPS / RTT A/B（`benchmark/`）

只做**性能**分析。采集由 TR3000 上 `./xpost-test` 写 `formal.*`；笔记本现查，不负责烧录或功能 ACK。

- **`just_tx`**：扫速找 ~5% 丢包工作点后逐包刺激/回包 → `formal.just_tx_config` / `just_tx_data`；`kinds` 含 **c**（RTT）与 **a/g/t**（固定 pps → `just_tx_run` + agg/gauge/trace_summary）。Profile：`just-tx-{c,a,g,t}` / `just-tx-hal-*`
- **`just_drop`**：`kinds` 含 **c**（满速 → `just_drop_config`/`data`）与 **a/g/t**（固定 pps → `just_drop_run` + agg/gauge/trace_summary）。Profile：`just-drop-{c,a,g,t}` / `just-drop-hal-*`（HAL = Pass XDP + app `drop on`）
- 笔记：`just_tx.ipynb` / `just_drop.ipynb`；环境与表说明见 `benchmark.utils` 包 docstring

```bash
cd benchmark && pixi install && pixi run ensure-schema && pixi run kernel
# 外部常驻 Jupyter Lab；内核选 xpost-benchmark (pixi)
```

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

### Perf (`ext::perf` / ingress program)

The crate mounts a private `xdp::ingress::XdpProg` implementing `xpost_type::XdpProg`
(re-exported as `esp_radio::wifi::XdpProg` under the `xdp` feature). Apps do **not**
see `XDP_PROG`; they read via `ext::perf::counter()` / `aggregate()` / `gauge()` /
`trace()`.

- `esp-radio` — `xpost::esp_radio::new` → `wifi::new(device, config, &ingress::XDP_PROG)`
- `perf-counter` — Counter storage + `EspRadioTrace` on the mounted prog
- `perf-aggregate` — latency aggregates + stretch histogram
- `perf-gauge` — current/peak gauges
- `perf-trace` — heapless event ring

Traits live in the `xpost-type` crate so esp-radio (xdp feature) and xpost share
one declaration (`EspRadioTrace: XdpProg`). esp-radio owns RX / `XdpAction`→`Action`
mapping / EspRadioTrace call-sites; `XdpProg::run` only invokes the leased XDP image.
App fields like `net_rx` stay in the firmware crate.

Optional / incomplete: `ext::ringbuf` behind Cargo feature `ringbuf` (currently
`compile_error!`).

