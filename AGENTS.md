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

## Scope

Bare-metal only (no ESP-IDF). Static loading of XDP ELF image bytes into slots;
no on-device ELF parse of the firmware image. Preserve ABI / linker layout with
`libbpf-mcu` XPM1 and board `partitions.csv`.
