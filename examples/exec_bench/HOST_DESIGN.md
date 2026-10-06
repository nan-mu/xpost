# exec_bench — host-side design

Firmware is an **opaque carrier**: `SerialHandler` + map/perf/exec verbs.
Workload opcodes and buffer layouts are **not** App/serial types — they are
interpreted by the XDP image (or HAL `run_hal`) and by **xpost-test parsing**
of map bytes.

Wired into `xpost-test` as `run = ["exec_bench"]` (`exam/exec_bench.rs`,
`config/exec_bench.toml`, `sql/exec_bench.sql`).

## Draft artifacts

| File | Role |
|------|------|
| [`exec_bench.rs`](exec_bench.rs) | just_*–style module rustdoc (both arms + parse); no harness |
| [`exec_bench.sql`](exec_bench.sql) | `exec_bench_config` (incl. `input BYTEA`) / `exec_bench_data` |

## Pipelines

### eBPF arm (`exec-bench-ebpf`)

```text
exec_bench (ebpf):
  agent flash fw exec-bench-ebpf
  for workload in {incr, square, bitswap, fib, buf, memcpy, fletcher32, bsort}:
    agent flash xdp workload
    agent clear perf
    agent update map workload          # key/value = opaque bytes
    agent enable exec                  # App: map[0] → invoke → map[1]
    agent get finish                   # UART OK finish from eBPF bpf_printk
    agent disable exec
    agent get perf
    agent get map                      # host parses output bytes
    upload to db
```

### HAL arm (`exec-bench-hal`)

Same control-plane verbs; no XDP flash. `OK finish` is a raw UART line from
the HAL App (twin of `bpf_printk`), not `OkReply::Finish`.

```text
exec_bench (hal):
  agent flash fw exec-bench-hal
  for workload in {incr, square, bitswap, fib, buf, memcpy, fletcher32, bsort}:
    agent clear perf
    agent update map workload
    agent enable exec                  # App: map[0] → run_hal → map[1] + OK finish
    agent get finish
    agent disable exec
    agent get perf
    agent get map
    upload to db
```

## Contracts

- Profiles: `[[fw]] exec-bench-ebpf` + per-workload `[[xdp]]`; `[[fw]] exec-bench-hal`
  in [`profiles.toml`](../../profiles.toml)
- `enable/disable exec` / `GetFinish` are `xpost-serial` verbs; agent mirrors
  them (`/v1/enable/exec`, `/v1/disable/exec`, `/v1/get/finish`)
- **Map wire**: xpost-serial `UpdateMap` / get-map blob entries carry **opaque
  byte tokens** only — no opcode structs in serial
- Map name `workload`: keys `0`=input / `1`=output; value 32-byte blob
- **`OK finish`**: unsolicited UART line (eBPF `bpf_printk` or HAL App raw
  write) — **not** serial `OkReply::Finish`
- Default perf kinds: **cagt** (counter, aggregate, gauge, trace)
- Unsolicited `OK init` still comes from serial server start (settle)

## Host / xpost-test parse (structure lives here)

When wiring the harness, decode map **value bytes** (not firmware enums):

```text
offset  size  field
0       4     opcode   (u32 LE)
4       4     param    (u32 LE)
8       24    payload / scratch   (32-byte value total)
```

Opcode names ↔ LE u32 values and expected output checks belong in the
xpost-test exam module (draft: [`exec_bench.rs`](exec_bench.rs); HAL
compute is [`run_hal`](../exec_bench_hal/src/lib.rs)). The App never sees
these names.

`update map` input for each cell: host builds the 32-byte blob (or a wire
token whose UTF-8 bytes are that blob / a prefix) and stores the same bytes
in `exec_bench_config.input`. After `get map`, parse the dump / entry value
into `exec_bench_data.output`.

## Future xpost-test wiring (not this phase)

1. `config/exec_bench.toml` + `FileConfig` + validate `"exec_bench"`
2. `dispatch` + load `sql/exec_bench.sql` in `formal::ensure`
3. Loop as above via `AgentClient` (branch on `firmware`: ebpf vs hal)
4. Parse map bytes → ok / output columns
5. `run = ["exec_bench"]` → rebuild → bare `./xpost-test`
