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
- **`xpost-serial`**: library only — **API** for `Session` / `install_xdp` /
  `program` / `request` / `request_report` / `Sink` (see that crate’s rustdoc).
  Cold install is **by XDP name**; do not pass raw cold addresses on the normal
  path. Perf metrics live in `xpost::ext::perf`; DUT serial read face is
  `report` (binary, length → feature mask) + `perf clear` (agent:
  `/v1/perf_clear`). Formal decodes the report and writes exam-scoped tables
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

## Commands

Mac（USB 接 ESP；cwd = `xpost/`）：只需常驻 agent。收到 `FlashProfile` 时按 `profiles.toml` 的 `build` 编译再烧录。

HTTP：`/health`、`/v1/*`（harness）、`/mcp`（Cursor Streamable HTTP MCP）。

```bash
cargo xtask agent --listen 0.0.0.0:9240 --no-monitor
```

Cursor MCP 示例 URL：`http://127.0.0.1:9240/mcp`。
Wi-Fi 凭据仍在 `examples/just_xdp/.env`、`examples/just_drop_hal/.env` 与 `examples/just_tx_hal/.env`（`WIFI_SSID` / `WIFI_PASS`）。

TR3000（编译期嵌入 `xpost-test/config/`：`default.toml` + `just_tx.toml` + `just_drop.toml`）：

**用户说「测试 / 跑一下 / 验证」时：默认正式跑。** 永远无参 `./xpost-test`；改
实验改 `config/*.toml` 后重编。Mac 侧首选 fish helper（build → scp → ssh 无参跑）：

```bash
fish -c 'source ~/.config/fish/functions/xpost-test.fish; and xpost-test'
```

**输出**：stdout/stderr 直接看终端；不要 `> /tmp/…`、`tee`、`tail` 截断日志文件。长跑用
`block_until_ms: 0` 后台跑，再读终端输出。

**正式性能测试永远只跑无参二进制**——不要环境变量、不要子命令、不要任何 CLI flags。
改实验：改 TOML → 重编 → scp → 无参 `./xpost-test`（或上面的 fish helper）。

手工等价步骤（一般不必；优先 fish helper）：

```bash
cd xpost-test && cargo build -p test --release
scp -O target/aarch64-unknown-linux-musl/release/xpost-test tr3000:~/xpost-test
# on TR3000:
./xpost-test
```

- `config/default.toml`：`run = [...]`（`just_tx` / `just_drop` / `dev` /
  `formal-clear`）、`[postgres]`、`[agent]`、`[dut]`、`[probe]`
- `config/just_tx.toml` / `just_drop.toml`：`formal`、`arms`（`xdp`/`hal`）；流水线
  固定再跑 `xdp_t`（`window_s=30`；全网格；无 OP 早停）
- XDP 固件工程：`examples/just_xdp`；profiles `just-*-cag` / `just-*-t` /
  `just-*-hal-cag`（agent `profiles.toml`）
- 调试火焰图 Trace（**火焰轨**）：`run = ["dev"]`（硬编码：XDP × `just-tx-t` × 100 pps × 2s × 1
  轮；**无 DB**；report → `/tmp/xpost-dev-flame.report`；Mac scp 到
  `target/dev-flame.report` 后 `pixi run python -m utils.export_flame_svg`）
- Agent HTTP：`/health`、`/v1/*`（含 `/v1/perf_clear`、`/v1/serial_window`、`/v1/map/{name}`）、`/mcp`
- 实验步骤与全流程：`xpost-test` 的 `overview` / `config` rustdoc；串口 API：`xpost-serial`
- 正式 `just_tx` 扫速网格：`400…4000` pps（步长 400）
- 正式 `just_drop`：同网格；`map dump counter`（IPv4 src 末字节分桶）→ `just_drop_data`
- Schema：`xpost-test/sql/formal.sql` + `just_drop.sql` / `just_tx.sql`；显式清空：
  `run = ["formal-clear"]` 后重编再跑

只跑正式 just_tx：`run = ["just_tx"]`。一次跑全部 formal A/B：`run = ["just_drop", "just_tx"]`。

可选手工 XDP 冷表操作：`cargo xtask agent program|list|clear|request|probe …`。

### Formal 采集与 `benchmark/` 分析立场

采集：TR3000 `./xpost-test` → `formal.*`（DDL：`xpost-test/sql/formal.sql`）。`benchmark/`
**不负责**烧录 / 刺激 / USB；也不再维护「一实验一 notebook」的 Evaluation 路径。

- **实验**（`just_tx` / `just_drop` / 后续）只是取数窗口；Analysis / Evaluation 应按
  **指标与 xpost 功能问题**横切（例如 `hal_agg`/`xdp_agg` 同名段在 Pass vs Drop vs TX
  下是否同事），常跨多个 exam 对比。见 `benchmark/README.md`。
- 近期：先跑完全部 formal 实验；数据仅作轻量健全性检查。深度作图与章节写作延后，
  且应对齐 Methodology 的功能分节，而非实验名。
- `benchmark/backup/` 冻结归档，勿改删。

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
`trace()` (four public exits). `report` / `clear_perf` are shared plumbing.
EspRadioTrace wiring lives in `ext/perf/trace/` beside the ring store.

- `esp-radio` — `xpost::esp_radio::new` → `wifi::new(device, config, &ingress::XDP_PROG)`
- `perf-counter` — Counter storage + `EspRadioTrace` on the mounted prog
- `perf-aggregate` — latency aggregates + stretch histogram (high pps)
- `perf-gauge` — current/peak gauges
- `perf-trace` — categorized event rings (sole timeline for flames)

Traits live in the `xpost-type` crate so esp-radio (xdp feature) and xpost share
one declaration (`EspRadioTrace: XdpProg`). esp-radio owns RX / `XdpAction`→`Action`
mapping / EspRadioTrace call-sites; `XdpProg::run` only invokes the leased XDP image.
App fields like `net_rx` stay in the firmware crate.

Optional / incomplete: `ext::ringbuf` behind Cargo feature `ringbuf` (currently
`compile_error!`).

