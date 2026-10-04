# exec_bench (eBPF arm)

Function-call eBPF compute via `xpost::ext::exec_call` and App `enable exec`.

- Cold image: `exec_bench.elf` (rebuild with `ebpf/build.sh`)
- Map `workload`: input/output keys; value 32-byte blob
- App: `SerialHandler` — `enable/disable exec`, map/perf (cagt), `OK init`
- Completion: eBPF `bpf_printk` → UART `OK finish` (host `GetFinish`)
- Host draft: [`HOST_DESIGN.md`](HOST_DESIGN.md), [`exec_bench.rs`](exec_bench.rs), [`exec_bench.sql`](exec_bench.sql)

```bash
cd examples/exec_bench && cargo build --release
# cold XDP image:
cd ebpf && ./build.sh
```

Flash: agent `POST /v1/flash/fw {"name":"exec-bench-ebpf"}` (co-flashes default xdp `incr`).
Per-workload: `POST /v1/flash/xdp {"name":"square"}` (etc.; shared elf for now).
