# XPOST EXB2 firmware

The firmware invokes the loaded XDP image once per EXB2 buffer. AOT and JIT
use the native build. Use `--features interp` for rBPF.

```sh
cargo build --release --locked
cargo build --release --locked --features interp --target-dir target/rbpf
```

See the [medium procedure](../../xpost-test/procedures/medium.md) for formal runs.

## Diagnostics

Build with `--features diag` for the legacy 32-byte workload commands and
performance report. Add `interp` for interpreter diagnostics. These commands
support the
[workload benchmark](../../xpost-test/scripts/workload_bench.py),
[workload sweep](../../xpost-test/scripts/sweep_all3.py), and
[interpreter benchmarks](../../xpost-test/scripts/interp_engine_bench.py).
