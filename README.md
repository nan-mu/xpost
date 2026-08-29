# xpost

**xpost** — XDP datapath on MCU (*express post*): AOT invoke, maps, helpers, IRAM slots, RX ingest.

ESP32-C3 / RV32IMC is the first instantiation. This crate does not claim architecture-agnostic behavior until additional targets exist.

## Layout

```
xpost/                          # this repo (publishable library at root)
  src/                          # xpost runtime
  xpost-build/                  # board codegen (layout/mem/bpf.toml → Rust + IRAM linker script)
  xtask/                        # root dispatcher only
  examples/
    firmware/                   # full DUT (WiFi, control plane, test0) — copyable MCU project
      xtask/                    # board-local doctor / map / gen
      layout.toml mem.toml bpf.toml config.toml
```

- **Library**: does not ship board tomls. Integrators set `XPOST_BOARD` to a directory containing `layout.toml` / `mem.toml` / `bpf.toml`.
- **Each example**: independent MCU project with its own xtask; can be copied out of tree.

## Commands

From repo root (host):

```bash
cargo xtask firmware doctor    # layout.toml → mem.toml
cargo xtask firmware map
cargo xtask firmware check     # MCU check inside examples/firmware
cargo xtask firmware build --release
```

Or inside the example:

```bash
cd examples/firmware
cargo xtask doctor
cargo fwrun                    # flash + monitor
```

## Integrator API (runtime)

Board wiring (WiFi, UDP control server, bench protocol) lives in **examples**, not in this crate.

| Area | Entry points |
|------|----------------|
| Datapath gate | `xpost::datapath::enable` / `disable` / `is_active` (re-exported at crate root) |
| RX program | `xpost::load_boot_image`, `xpost::swap_rx`, `xpost::hot_swap_rx`, `xpost::try_rx_slot` |
| Maps (userspace) | `xpost::map::init`, `init_firmware_maps`, `register_map`, `register_ringbuf`, `with_*_map`, `peek_array_u64` |
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

Copy into `examples/firmware/`:

- `esp32c3.bin`
- `bpf.toml` (with `aot_addr` after MCU report merge)

`mem.toml` is produced by `cargo xtask firmware doctor` and consumed by libbpf-mcu (`BS_FIRMWARE_MEM_MAP` / default `xpost/examples/firmware/mem.toml`).
