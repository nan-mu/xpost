# xpost-test compile-time config

Embedded by `common` at build time (`include_str!`). Edit → `cargo build --release` → scp `xpost-test` to TR3000.

| File | Role |
|------|------|
| `default.toml` | `run = [...]`, `[postgres]`, `[agent]`, `[dut]`, `[probe]` |
| `just_tx.toml` | `formal`, `arms` (`["xdp"]` for XDP-only) |
| `just_drop.toml` | same |
| `firewall.toml` | `formal`, `arms`, `sizes`, `traffic` |

The default `run = ["medium"]` checks medium readiness. Use it alone.
See the [medium procedure](../procedures/medium.md).

Explicit experiment selections are `just_tx`, `just_drop`, `firewall`, `exec_bench`, `case`, `case-smoke`, and `medium-plan`.
Use `dev` or `lookup` for diagnostics. Use `formal-clear` only for an explicit database clear.

SQL DDL: `../sql/{formal,just_drop,just_tx,firewall}.sql` (+ optional `dev.sql`, unwired).
