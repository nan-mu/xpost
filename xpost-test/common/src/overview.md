# xpost-test, the TR3000 formal and flame harness

This crate runs the experiment workflow.
The macOS Agent owns USB and serves HTTP under `/v1/*`.
The `xpost-serial` crate defines the wire types.

The runner uses `profile.toml` for selection and `config/` for experiment settings.
It reads `/root/.db` when it needs PostgreSQL.
A new collection reconciles managed Protocol V1 tables in `release`.
Recreating a table with incompatible columns removes that table's data.
See the [experiment database procedure](../../procedures/database.md).

# Runner entry

Run `./xpost-test` on TR3000 without arguments or additional environment variables.
Change `run` in `profile.toml` before building.

| Selection | Operation |
|-----------|-----------|
| `all`, `medium` | Campaign after readiness checks |
| `all-plan`, `medium-plan` | Print the plan without connecting to PostgreSQL |
| `all-preflight`, `medium-preflight` | Check deployment and collector readiness |
| `db-preflight` | Check Protocol V1 tables and privileges without the Agent |
| `exec_bench` | Run standalone EXB2 collection with Protocol V1 |
| `dev` | Run HAL smoke on TX, DROP, and firewall without PostgreSQL |

See the [medium procedure](../../procedures/medium.md) for the campaign plan.

# Topology

```text
gateway (DHCP)
        │
Mac Mini, TR3000, and ESP32-C3 on one subnet
        │
   TR3000 ──2.4G── ESP32-C3 STA
     RAW + TC; agent on Mac flashes DUT
```

# Stimulus contract

| Item | Convention |
|------|------------|
| Stim | UDP `dst = ESP_IP`, `dport` echo port (default 8080) |
| Payload | `FWSTIM` + `u64` LE `seq` |
| Timestamps | TC egress gives T0. A matching echo or bounce gives T1. |

# Build / deploy

```bash
cd xpost-test && cargo build --release
scp -O target/aarch64-unknown-linux-musl/release/xpost-test tr3000:~/xpost-test
```

## Router deps (TR3000)

```bash
opkg update && opkg install kmod-sched-bpf kmod-sched-core ip-full
ip link   # Check probe.iface (often br-lan)
```
