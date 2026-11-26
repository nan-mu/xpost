# HAL EXB2 firmware

The firmware runs the selected EXB2 kernel once per buffer. It uses the same
serial staging and Metrics commands as [XPOST EXB2 firmware](../exec_bench).

The build needs RISC-V Clang in `PATH`. Set `EXB2_CLANG` to select a compiler.

```sh
cargo build --release --locked
```

## Diagnostics

Build with `--features diag` for the boot-time scale sweep. The
[workload sweep script](../../xpost-test/scripts/sweep_all3.py) reads these logs.
