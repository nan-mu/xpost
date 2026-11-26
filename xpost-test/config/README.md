# xpost-test configuration

The runner embeds experiment settings at build time.
Change the selection in `../profile.toml`. Rebuild before deployment.

| File | Settings |
|------|----------|
| `../profile.toml` | Selection, phase, resume |
| `default.toml` | Agent, DUT, probe |
| `firewall.toml` | Firewall blacklist size |
| `exec_bench.toml` | Standalone EXB2 modes |

See the [campaign procedure](../procedures/medium.md) for selection details.
The runner reads database settings at runtime. See the [database procedure](../procedures/database.md).
