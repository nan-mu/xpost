# exec_bench — host-side design draft

Firmware uses `ext::exec_call` + `SerialHandler` (`enable/disable exec`, map
`workload`). Host draft artifacts live in this directory; **do not** wire
`xpost-test/**` yet.

## Draft artifacts

| File | Role |
|------|------|
| [`exec_bench.rs`](exec_bench.rs) | just_*–style module rustdoc (pipeline + fields); no harness |
| [`exec_bench.sql`](exec_bench.sql) | `exec_bench_config` (incl. `input BYTEA`) / `exec_bench_data` |

## Pipeline (xdp/exec only; no HAL)

```text
exec_bench:
  agent flash fw fw
  for workload in {incr, square, bitswap, fib, buf, memcpy, fletcher32, bsort}:
    agent flash xdp workload
    agent clear perf
    agent update map workload
    agent enable exec
    agent get finish
    agent disable exec
    agent get perf
    agent get map
    upload to db
```

## Contracts

- Profiles: `[[fw]] exec-bench-ebpf` + per-workload `[[xdp]]` in repo
  [`profiles.toml`](../../profiles.toml) (shared `exec_bench.elf` for now)
- `enable/disable exec` / `GetFinish` are `xpost-serial` verbs; agent mirrors
  them (`/v1/enable/exec`, `/v1/disable/exec`, `/v1/get/finish`)
- **`OK finish`**: emitted by eBPF `bpf_printk` onto UART — **not** by
  `SerialHandler` / `write_ok(Finish)`
- Map `workload`: keys `0`=input / `1`=output; value 32-byte Compact ELF blob
- Default perf kinds: **cagt** (counter, aggregate, gauge, trace)
- Unsolicited `OK init` still comes from serial server start (settle)

## Future xpost-test wiring (not this phase)

1. `config/exec_bench.toml` + `FileConfig` + validate `"exec_bench"`
2. `dispatch` + load `sql/exec_bench.sql` in `formal::ensure`
3. Loop as above via `AgentClient`
4. `run = ["exec_bench"]` → rebuild → bare `./xpost-test`
