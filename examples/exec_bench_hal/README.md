# exec_bench_hal (native HAL arm)

Native reference twins for `exec_bench` (same buffer layout, opcodes, UART protocol).
No XDP lease; times `exec_bench_common::run_hal` with `mcycle`.

App settle: unsolicited `OK init`. `bench …` is experiment-local (raw UART).

```bash
cargo run --manifest-path xtask/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')" -- \
  exec_bench_hal build --release
```

Flash: agent `POST /v1/flash/fw {"name":"exec-bench-hal"}`.
