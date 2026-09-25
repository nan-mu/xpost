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

```bash
cargo xtask just_drop build --release
cargo xtask just_drop_hal build --release

# Mac agent (cwd = xpost/; FlashProfile uses profiles.toml + built firmware)
cargo xtask agent --listen 0.0.0.0:9240 --no-monitor

# TR3000: single binary from xpost-test (config baked at compile time)
cd xpost-test && cargo build -p test --release
scp target/aarch64-unknown-linux-musl/release/xpost-test tr3000:~/xpost-test
# on TR3000:
./xpost-test                 # default_bench=just-drop-pps → flash HAL/XDP, formal.experiment_*
./xpost-test --smoke
./xpost-test just-drop-pps --path hal
./xpost-test --help-benches

cargo xtask agent program --elf examples/just_drop/just_drop.elf
cargo xtask agent list
cargo xtask agent clear
cargo xtask agent request PING
cargo xtask agent request counters
cargo xtask agent probe <esp-ipv4>
```

`just-drop-pps` asks the agent to `FlashProfile` `just-drop-hal` / `just-drop` (firmware + `xdp.elf`) each arm, then samples `drop` into Postgres schema `formal`. `ERR unknown` on `drop on` means HAL firmware was not flashed.

Wi-Fi STA needs `WIFI_SSID` / `WIFI_PASS` in `examples/just_drop/.env` and `examples/just_drop_hal/.env`.

## Control: `just_drop_hal`

crates.io-only esp-hal / embassy-net / smoltcp — **no** path deps into xpost, **no** cold XDP. Serial: `drop on` / `drop off` / `counters` / `rxcnt`.

### PPS A/B (`benchmark/`)

Pixi + Jupyter；`PGSCHEMA=formal`；数据经 `utils.db` 现查：

```bash
cd benchmark && pixi install && pixi run ensure-schema && pixi run lab
# notebook: just_drop_pps.ipynb
# TR3000: ./xpost-test   (writes formal.experiment_* directly)
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