# exec_bench (eBPF arm)

Opaque App carrier for function-call XDP via `xpost::ext::exec_call`.

- Cold images: `../build/<name>/<name>.elf` from `pixi run build -c <name>` in `libbpf-mcu`
- Map `workload`: keys `0`/`1`, value = opaque 32-byte blob
- App: `SerialHandler` — exec / map / perf (cagt); does **not** interpret opcodes
- Completion: eBPF `bpf_printk` → UART `OK finish` (host `GetFinish`)
- Host draft: [`HOST_DESIGN.md`](HOST_DESIGN.md), [`exec_bench.rs`](exec_bench.rs), [`exec_bench.sql`](exec_bench.sql)
- HAL twin (same pipeline, no XDP): [`../exec_bench_hal`](../exec_bench_hal)

```bash
cargo build --release
```

Flash: agent `POST /v1/flash/fw {"name":"exec-bench-ebpf"}` (co-flashes default xdp `incr`).
Per-workload: `POST /v1/flash/xdp {"name":"square"}` (and the other stems).
