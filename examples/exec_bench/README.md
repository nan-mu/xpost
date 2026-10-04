# exec_bench (eBPF arm)

Opaque App carrier for function-call XDP via `xpost::ext::exec_call`.

- Cold image: `exec_bench.elf` (rebuild with `ebpf/build.sh`)
- Map `workload`: keys `0`/`1`, value = opaque 32-byte blob
- App: `SerialHandler` — exec / map / perf (cagt); does **not** interpret opcodes
- Completion: eBPF `bpf_printk` → UART `OK finish` (host `GetFinish`)
- Host draft (pipelines + **xpost-test parse**): [`HOST_DESIGN.md`](HOST_DESIGN.md),
  [`exec_bench.rs`](exec_bench.rs), [`exec_bench.sql`](exec_bench.sql)
- HAL twin (same pipeline, no XDP): [`../exec_bench_hal`](../exec_bench_hal)

```bash
cd examples/exec_bench && cargo build --release
# cold XDP image:
cd ebpf && ./build.sh
```

Flash: agent `POST /v1/flash/fw {"name":"exec-bench-ebpf"}` (co-flashes default xdp `incr`).
Per-workload: `POST /v1/flash/xdp {"name":"square"}` (etc.; shared elf for now).
