# exec_bench (eBPF arm)

Function-call eBPF compute via `xpost::ext::exec_call` and App `enable exec`.

- Cold image: `exec_bench.elf` (rebuild with `ebpf/build.sh`)
- Map `workload`: input/output keys; value 32-byte blob
- App: `SerialHandler` — `enable/disable exec`, map/perf (cagt), `OK init`
- Completion: eBPF `bpf_printk` → UART `OK finish` (host `GetFinish`)
- Host draft: [`HOST_DESIGN.md`](HOST_DESIGN.md), [`exec_bench.rs`](exec_bench.rs), [`exec_bench.sql`](exec_bench.sql)

```bash
# from repo root (use host triple for xtask on non-Mac):
cargo run --manifest-path xtask/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')" -- \
  exec_bench build --release
```

Flash: agent `POST /v1/flash/fw {"name":"exec-bench-ebpf"}` (co-flashes default xdp `incr`).
Per-workload: `POST /v1/flash/xdp {"name":"square"}` (etc.; shared elf for now).
