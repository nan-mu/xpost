# xpost-test compile-time config

Embedded by `core` at build time (`include_str!`). Edit → rebuild → scp `xpost-test` to TR3000.

| File | Role |
|------|------|
| `default.toml` | `run = [...]`, `[postgres]`, `[agent]`, `[dut]`, `[probe]` |
| `just_tx.toml` | `formal`, `arms` (`["xdp"]` for XDP-only) |
| `just_drop.toml` | same |
| `firewall.toml` | `formal`, `arms`, `sizes`, `traffic` |

Allowed `run` values: `just_tx` | `just_drop` | `firewall` | `dev` | `formal-clear`.

SQL DDL: `../sql/{formal,just_drop,just_tx,firewall}.sql` (+ optional `dev.sql`, unwired).
