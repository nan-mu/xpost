# exec_bench (eBPF arm)

Function-call eBPF execution benchmark using `xpost::ext::exec_call`.

- Cold image: `exec_bench.elf` (rebuild with `ebpf/build.sh`)
- Workloads / UART protocol: shared with HAL via `../exec_bench_common`
- Host-side design draft (xpost-test / agent / perf): [`HOST_DESIGN.md`](HOST_DESIGN.md)

```bash
# from repo root (Linux host triple for xtask):
cargo run --manifest-path xtask/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')" -- \
  exec_bench build --release
```
