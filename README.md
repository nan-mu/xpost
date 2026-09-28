# xpost

**xpost** — XDP datapath on MCU (*express post*): AOT invoke, maps, helpers, IRAM slots, RX ingest.

ESP32-C3 / RV32IMC is the first instantiation. This crate does not claim architecture-agnostic behavior until additional targets exist.

## Layout

```
xpost/                          # this repo (publishable library at root)
  src/                          # xpost runtime
  xpost-build/                  # board codegen (layout/partitions → Rust + IRAM linker script)
  xtask/                        # root dispatcher only
  examples/
    just_drop/                  # firmware-first cold XDP ELF path
```

- **Library**: does not ship board tomls. Integrators set board metadata / `partitions.csv` via the firmware crate.
- **Each example**: independent MCU project with its own xtask; can be copied out of tree.
- **`mem.toml` is obsolete**. Codegen consumes in-memory layout from `[package.metadata.xpost]` ∪ board files. The libbpf-mcu **contract** `layout.toml` (§3.2) is exported under `target/layout-export/`.

## Boot

Firmware-first only: `XdpRt::init` scans Flash Compact ELF (XDP ELF), creates required maps; `lease` installs VMR aliases from image meta.

Soft errors / driver Observe counters: enable `trace-counter` and read `xpost::XDP_PROG`. Wi-Fi+XDP: `esp-radio` feature + `xpost::esp_radio::new`. Helper timestamps: `trace-timestamp`. Control-plane APIs return `Result`.

## Commands

From repo root (host):

```bash
cargo xtask just_drop build --release
cargo xtask agent program --elf examples/just_drop/just_drop.elf
```

## AOT handoff (from libbpf-mcu)

Build / encode an XDP ELF (host: `xpost-compact-elf` + libbpf-mcu `bpf.toml` for map meta). Cold-install via agent. For firmware layout export: `cargo xtask sync export --board <example>`.
