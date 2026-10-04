# exec_bench (eBPF arm)

Function-call eBPF execution benchmark using `xpost::ext::exec_call`.

- Cold image: `exec_bench.elf` (rebuild with `ebpf/build.sh`)
- Workloads / UART protocol: shared with HAL via `../exec_bench_common`
- App settle: unsolicited `OK init`; `bench …` is experiment-local (raw UART)
- Host-side design: [`HOST_DESIGN.md`](HOST_DESIGN.md)

```bash
# from repo root (use host triple for xtask on non-Mac):
cargo run --manifest-path xtask/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')" -- \
  exec_bench build --release
```

Flash: agent `POST /v1/flash/fw {"name":"exec-bench-ebpf"}` (co-flashes `exec_bench` xdp).
