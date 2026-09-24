# xpost

Rust MCU XDP runtime (crate `xpost`) plus board examples and host tooling.
Phase 1: ESP32-C3 / RV32IMC.

## Artifact names (do not confuse)

| Name | What it is | Typical path |
|------|------------|--------------|
| **Firmware ELF** | Board app (`XdpRt`, Wi-Fi, …) | `examples/<app>/…/release/<app>` |
| **XDP ELF** (`xdp.elf`) | Cold-region XDP container (`.xpost.meta` + `.xpost.image`) | `../build/<stem>/<stem>.elf` |
| **`esp32c3.elf`** | libbpf-mcu AOT debug ELF (not for cold `write-bin`) | `../build/<stem>/esp32c3.elf` |

Prefer the term **XDP ELF** / **`xdp.elf`** in docs and logs (not “Compact ELF”).
Cold flash target for just_drop is `TABLE.0` / `0x310000`; file size must fit
`image_capacity` (just_drop: `0x1000`).

## Layout

- Root crate `xpost`: board-agnostic runtime (maps, helpers, IRAM slots, RX ingest).
- `examples/<app>/`: self-contained MCU projects.
- Host (workspace `exclude`, own triple): `xpost-serial` (library), `xpost-agent`
  (CLI that exercises the library), `xpost-build`, `xtask`.

## Host serial

- **`xpost-serial`**: library only — `Session`, `write_bin` / `program` / `request`, `Sink`.
- **`xpost-agent`**: `cargo xtask agent write-bin|program|request` → calls the library.
  Default cold write-bin path/addr match just_drop XDP ELF @ `0x310000`.

## Commands

```bash
cargo xtask just_drop build --release
cargo xtask agent write-bin --port <PATH> --elf <xdp.elf> --addr 0x310000
cargo xtask agent program --firmware <fw.elf> --partitions <csv> [--xdp-elf <xdp.elf>]
```

## Scope

Bare-metal only (no ESP-IDF). Static loading of XDP ELF image bytes into slots;
no on-device ELF parse of the firmware image. Preserve ABI / linker layout with
`libbpf-mcu` XPM1 and board `partitions.csv`.
