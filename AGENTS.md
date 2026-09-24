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
- Host (workspace `exclude`, own triple): `xpost-serial` (library), `xpost-agent`
  (CLI), `xpost-build`, `xtask`.

## Host serial / agent

- **`xpost-serial`**: library only — `Session`, `install_xdp` / `list_xdp` / `program` /
  `request`, `Sink`. Cold install is **by XDP name** (overwrite same name, else append);
  do not pass raw cold addresses for the normal path.
- **`xpost-agent/`** (crate `xpost-agent`): CLI. Dispatch stays `cargo xtask agent …`
  (`--manifest-path xpost-agent/Cargo.toml`). Agent uses `xpost-elf` to read the
  program name from the XDP ELF, then calls `Session::install_xdp(name, path)`.
- **`--port`**: optional. Omit → pick the unique USB serial port; if zero or many,
  error and require `--port`.

## Commands

```bash
cargo xtask just_drop build --release

# Install / overwrite just_drop XDP ELF in the cold table (default just_drop.elf)
cargo xtask agent program --elf /Users/nan/bs/build/just_drop/just_drop.elf

# List cold-table entries
cargo xtask agent list

# Erase all XDP ELF containers in the cold table
cargo xtask agent clear

cargo xtask agent request PING
```

## Scope

Bare-metal only (no ESP-IDF). Static loading of XDP ELF image bytes into slots;
no on-device ELF parse of the firmware image. Preserve ABI / linker layout with
`libbpf-mcu` XPM1 and board `partitions.csv`.
