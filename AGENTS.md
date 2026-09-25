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
- **`xpost-test/`**: experiment director. It decides the scenario (load drop or not,
  send a stimulus, expect `ack` or `no-ack`, then read counters). It calls
  `xpost-agent` / `xpost-serial`; it does not own the USB session itself while
  the agent is running.
- **`--port`**: optional. Omit → pick the unique USB serial port; if zero or many,
  error and require `--port`.

## Commands

Mac（USB 接 ESP；cwd = `xpost/`）：只需常驻 agent。收到 `FlashProfile` 时按 `profiles.toml` 的 `build` 编译再烧录。

```bash
cargo xtask agent --listen 0.0.0.0:9240 --no-monitor
```

Wi-Fi 凭据仍在 `examples/just_drop/.env` 与 `examples/just_drop_hal/.env`（`WIFI_SSID` / `WIFI_PASS`）。

TR3000（编译期嵌入 `xpost-test/config/test.toml`）：

```bash
cd xpost-test && cargo build -p test --release
scp target/aarch64-unknown-linux-musl/release/xpost-test tr3000:~/xpost-test
# on TR3000 — 性能测试（formal.experiment_*）：
./xpost-test
./xpost-test just-drop-pps --path hal   # 可选：只跑一臂
./xpost-test --help-benches
```

`--smoke` 仅开发调试（隐藏 flag），正式性能采集不要使用。

可选手工 XDP 冷表操作：`cargo xtask agent program|list|clear|request|probe …`。

### PPS A/B（`benchmark/`）

只做**性能**分析（均值 PPS / CV / Allan·噪声模型脚手架）。采集由 `./xpost-test` 写 `formal.experiment_*`；笔记本现查，不负责烧录或功能 ACK。

```bash
cd benchmark && pixi install && pixi run ensure-schema && pixi run lab
# notebook: just_drop_pps.ipynb
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