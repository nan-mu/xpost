# exec_bench_hal (native HAL arm)

Same host pipeline as `exec_bench` (map / enable exec / `OK finish` / perf),
without an XDP lease. `enable exec` runs `exec_bench_common::run_hal` on the
opaque map input bytes and writes a raw UART `OK finish` line (twin of eBPF
`bpf_printk`).

See [`../exec_bench/HOST_DESIGN.md`](../exec_bench/HOST_DESIGN.md) for the full
pipeline and where opcode/layout parsing belongs (xpost-test, not serial).

```bash
cd examples/exec_bench_hal && cargo build --release
```

Flash: agent `POST /v1/flash/fw {"name":"exec-bench-hal"}`.
