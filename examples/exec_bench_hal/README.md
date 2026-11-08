# exec_bench_hal (native HAL arm)

Same host pipeline as `exec_bench` (map / enable exec / `OK finish` / perf),
without an XDP lease. `enable exec` runs [`run_hal`](src/lib.rs) on the
opaque map input bytes, writes the result, records `xdp_end`, then logs
`OK finish` with `info!` (UART `INFO - OK finish`).

See [`../exec_bench/HOST_DESIGN.md`](../exec_bench/HOST_DESIGN.md) for the full
pipeline. Opcode checks live in xpost-test, not in serial.

```bash
cargo build --release
```

Flash: agent `POST /v1/flash/fw {"name":"exec-bench-hal"}`.
