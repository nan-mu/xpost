# exec_bench_hal (native HAL arm)

Native reference twins for `exec_bench` (same buffer layout, opcodes, UART protocol).
No XDP lease; times `exec_bench_common::run_hal` with `mcycle`.

```bash
cargo run --manifest-path xtask/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')" -- \
  exec_bench_hal build --release
```
