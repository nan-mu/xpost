# exec_bench_hal (native HAL arm)

Native reference twins for `exec_bench` (same buffer layout, opcodes).
No XDP lease; times `exec_bench_common::run_hal` with `mcycle`.

App settle: unsolicited `OK init`. Host draft for this arm is not written yet
(see `examples/exec_bench/HOST_DESIGN.md` for the xdp/exec arm).

```bash
cd examples/exec_bench_hal && cargo build --release
```

Flash: agent `POST /v1/flash/fw {"name":"exec-bench-hal"}`.
