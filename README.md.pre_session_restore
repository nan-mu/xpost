# xpost

**xpost** — XDP datapath on MCU (*express post*): AOT invoke, maps, helpers, IRAM slots, RX ingest.

ESP32-C3 / RV32IMC is the first instantiation. This crate does not claim architecture-agnostic behavior until additional targets exist.

## Layout

```
xpost/                          # this repo (publishable library at root)
  src/                          # xpost runtime
  xpost-build/                  # board codegen (layout/bpf.toml → Rust + IRAM linker script)
  xtask/                        # root dispatcher only
  examples/
    esp32c3-direct/             # firmware-first (default boot path)
    esp32c3-bpf-toml/           # from-bpf-toml boot path
      xtask/                    # board-local doctor / map / gen / layout-export
      layout.toml bpf.toml config.toml   # intent layout; mem.toml obsolete
```

- **Library**: does not ship board tomls. Integrators set `XPOST_BOARD` to a directory containing intent `layout.toml` (and `bpf.toml` when using `from-bpf-toml`).
- **Each example**: independent MCU project with its own xtask; can be copied out of tree.
- **`mem.toml` is obsolete**. Codegen consumes intent `layout.toml` in memory. The libbpf-mcu **contract** `layout.toml` (§3.2) is exported under `target/layout-export/` (see docs/book/xdp-runtime-plan.md §3.1–3.2).

## Boot paths

Firmware-first is the **default** (no Cargo feature). Opt into bpf.toml boot with `from-bpf-toml`.

| Mode | Feature | Example | Boot |
|------|---------|---------|------|
| Firmware-first (default) | _(none)_ | `esp32c3-direct` | inject + `lease` |
| bpf.toml | `from-bpf-toml` | `esp32c3-bpf-toml` | `boot_maps` / `boot_xdps` + lease |

Soft errors on the RX / helper path are exposed via `xpost::xdp::ERRORS` (atomics), not logs. Control-plane APIs return `Result`.

## Commands

From repo root (host):

```bash
cargo xtask firmware doctor    # validate intent; export contract layout.toml
cargo xtask firmware map
cargo xtask firmware check     # MCU check inside examples/firmware
cargo xtask firmware build --release
```

Or inside an example:

```bash
cd examples/esp32c3-direct
cargo xtask doctor             # → target/layout-export/layout.toml
cargo xtask layout-export
cargo xtask map
```

## Integrator API (runtime)

Board wiring (WiFi, UDP control server, bench protocol) lives in **examples**, not in this crate.

| Area | Entry points |
|------|----------------|
| Datapath gate | `xpost::datapath::enable` / `disable` / `is_active` (re-exported at crate root) |
| RX program | `xpost::load_boot_image`, `xpost::swap_rx`, `xpost::hot_swap_rx`, `xpost::try_rx_slot` |
| Maps (userspace) | `xpost::map::init`, `init_firmware_maps`, `MapRt` / `MAP_RT` (feature `ringbuf` incomplete — do not enable) |
| AOT map bind (load time) | `bind_map_symbols` / `load_boot_image` — keyed by **name**, VMA from link report only |
| Ingest | `xpost::driver::FirewallDevice` wraps an `embassy-net` driver and runs `xpost::xdp` on RX |

Control-plane opcodes (`hot_load`, `map_crud`) belong in firmware examples or a shared firmware crate (see `examples/esp32c3-firmware`).

### Drop bench UDP (`examples/esp32c3-drop-hal`, `examples/esp32c3-drop-xdp`)

Host probe sends 1-byte UDP requests to DUT `:8081`; replies go to `probe.reply_port` (24 bytes):

| Request | Meaning | Reply |
|---------|---------|-------|
| `0x00` | QUERY stats | 24 B: `drop_count`, `t_first_us`, `t_last_us` (u64 LE each) |
| `0x01` | RESET | clear counters, re-enable drop path (no reply) |

Host `STOP` (`0x02`) disables the drop path after blast (HAL: `drop_enabled = false`; XDP: `datapath::disable()`). `RESET` re-arms.

## AOT handoff (from libbpf-mcu)

Copy into a `from-bpf-toml` example board dir:

- `esp32c3.bin`
- `bpf.toml` (with `aot_addr` after MCU report merge)

For firmware-first, export the contract with `cargo xtask doctor` / `layout-export`. libbpf-mcu reads it via `BS_FIRMWARE_MEM_MAP` (env name retained) pointing at the contract `layout.toml` (default: `xpost/examples/esp32c3-direct/target/layout-export/layout.toml`).
