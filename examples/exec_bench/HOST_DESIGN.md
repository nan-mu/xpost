# exec_bench — host-side design draft (no code in this phase)

Firmware is implemented (`ext::exec_call`, `examples/exec_bench`, `examples/exec_bench_hal`).
This note records evaluations and the future `xpost-test` wiring **without modifying**
`xpost-test/**` or `xpost-agent/**`.

Baseline: config-only `xpost-test` (`just_tx` / `just_drop` / `dev` / `formal-clear`).

## 1. `ext::perf` — evaluation

| Need | Verdict |
|------|---------|
| µs aggregates (`xdp_execution`) | Too coarse for short ALU ops; brackets live on esp-radio RX |
| CPU cycles | **Missing** (PERFST host codec only; no DUT) |
| HAL-comparable `execution` | just_* HAL measures inactive Pass, not app compute |
| Wi-Fi-free | `perf-*` → `esp-radio/xdp-observe` |

**Chosen for firmware:** RV32 `mcycle` + UART text
`OK bench op=… iters=… cycles=… us=… ok=…`. Do not build Table-1 on `xdp_agg.execution`.

## 2. `xpost-agent` — evaluation / draft

| Capability | Enough? |
|------------|---------|
| `FlashProfile` (`exec-bench-ebpf` / `exec-bench-hal`) | Yes |
| `/v1/request` → `bench run …` | Yes |
| ready line `xpost ready exec_bench` | Yes |
| `perf_clear` / binary `report` | Not needed for primary metric |
| UDP probe | Not needed |

**Draft:** no new HTTP routes. Future harness uses `request("bench run …")` and parses
`OK bench …`. Agent code stays unchanged in this phase.

## 3. `xpost-test` — schema + pipeline draft (do not implement yet)

### Why not reuse just_* 

- just_* assume RAW UDP / TC / binary `report`.
- exec_bench is Wi-Fi-free and text-UART only.
- Extension pattern now: TOML + `dispatch` + `sql/*.sql` + `bench/*.rs` only
  (no suite/CLI/executor).

### Future DDL (`sql/exec_bench.sql`)

```sql
CREATE TABLE exec_bench_config (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  exam_id UUID NOT NULL REFERENCES exam(id) ON DELETE CASCADE,
  firmware TEXT NOT NULL CHECK (firmware IN ('ebpf', 'hal')),
  workload TEXT NOT NULL,
  iterations BIGINT NOT NULL,
  repeats INT NOT NULL,
  started_at TIMESTAMPTZ NOT NULL,
  stopped_at TIMESTAMPTZ
);

CREATE TABLE exec_bench_data (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  exam_id UUID NOT NULL REFERENCES exam(id) ON DELETE CASCADE,
  config_id UUID NOT NULL REFERENCES exec_bench_config(id) ON DELETE CASCADE,
  rep INT NOT NULL,
  cycles BIGINT NOT NULL,
  us BIGINT NOT NULL,
  ok BOOLEAN NOT NULL,
  UNIQUE (config_id, rep)
);
```

Use independent `firmware TEXT` (`ebpf`|`hal`); do not extend the just_* `firmware` enum.

### Future pipeline (`bench/exec_bench.rs`)

1. `config/exec_bench.toml` + `FileConfig` `include_str!` + validate `"exec_bench"`
2. `main.rs` `dispatch` branch
3. Load `sql/exec_bench.sql` in `formal::ensure`
4. Loop: flash profile → wait ready → `bench run` × workloads × repeats → INSERT
5. Usage: `run = ["exec_bench"]` → rebuild → bare `./xpost-test`

No Harness / RawTx / `write_perf` on the primary path.

### Serial contract (already on DUT)

```
xpost ready exec_bench
bench list
bench run <opcode> <iterations>
→ OK bench op=<opcode> iters=<n> cycles=<u64> us=<u64> ok=1
bench verify <opcode>
```

Opcodes: `incr` `square` `bitswap` `fib` `buf` `memcpy` `fletcher32` `bsort`.
