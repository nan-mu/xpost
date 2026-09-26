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

## Host serial / agent

- **`xpost-serial`**: library only — `Session`, `install_xdp` / `list_xdp` / `program` /
  `request`, `Sink`. Cold install is **by XDP name** (overwrite same name, else append);
  do not pass raw cold addresses for the normal path.
- **`xpost-agent/`** (crate `xpost-agent`): CLI. Dispatch stays `cargo xtask agent …`
  (`--manifest-path xpost-agent/Cargo.toml`). Agent uses `xpost-elf` to read the
  program name from the XDP ELF, then calls `Session::install_xdp(name, path)`.
  Long-running mode (`--listen`) serves HTTP JSON `/v1/*` and MCP `/mcp` on one
  port (shared USB session).
- **`xpost-test/`**: experiment director. It decides the scenario (load drop or not,
  send a stimulus, expect `ack` or `no-ack`, then read counters). It calls
  `xpost-agent` / `xpost-serial`; it does not own the USB session itself while
  the agent is running.
- **`--port`**: optional. Omit → pick the unique USB serial port; if zero or many,
  error and require `--port`.

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
scp target/aarch64-unknown-linux-musl/release/xpost-test tr3000:~/xpost-test
# on TR3000:
./xpost-test
```

- `config/default.toml`：`run = [...]`（跑哪些 bench）、`[postgres]`、`[agent]`、`[dut]`、`[probe]`、版本与 profiles
- `config/just_tx.toml`：`[pps]` / `[rtt]` 轮次、臂、formal
- `config/just_drop.toml`：`[pps]` 同上

只跑一个实验：`run = ["just-tx-rtt"]`。一次跑全部 formal A/B：`run = ["just-drop-pps", "just-tx-pps", "just-tx-rtt"]`。

可选手工 XDP 冷表操作：`cargo xtask agent program|list|clear|request|probe …`。

### PPS / RTT A/B（`benchmark/`）

只做**性能**分析。采集由 TR3000 上 `./xpost-test` 写 `formal.*`；笔记本现查，不负责烧录或功能 ACK。

- **just-tx-rtt**（`default.toml` 当前 `run`）：同一 `target_pps` 网格扫丢包 / return PPS（`phase=sweep`），再在约 5% 丢包工作点测 RTT（`phase=operating`）→ `formal.just_tx_rtt_*`
- **just-tx-pps** / **just-drop-pps**：满速 blast 下的处理 PPS → `formal.experiment_*`
- 笔记：`just_tx.ipynb`（RTT + 处理 PPS）、`just_drop.ipynb`（drop 处理 PPS）

```bash
cd benchmark && pixi install && pixi run ensure-schema && pixi run lab
# notebook: just_tx.ipynb / just_drop.ipynb
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

Optional / incomplete: `ext::ringbuf` behind Cargo feature `ringbuf` (currently
`compile_error!`).