# strcmp

The EXB2 task ID is 6. The Cargo feature is `strcmp`.
See the [Rust workload](../src/workloads/strcmp.rs) and the [original C kernel](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/exb2/strcmp.h).

## Task

Input contains two `n + 1` byte strings. Each ends with a NUL byte.
The task accepts `n` from 0 through 4096 and uses no Scratch.
Rust checks both final terminators before the first round.
It compares unsigned bytes until a difference, a shared NUL, or `n` pairs.
The final fixed NUL lies outside the comparison.
The result `lo` is the two's complement `u32` value of the first difference `a - b`. Equal strings return zero.
The result `hi` counts examined pairs, including the pair that triggered an early stop.

## Agent and runner

Select the `exec-bench-hal-strcmp` firmware profile.
The firmware accepts task 6 through the [shared EXB2 interface](../README.md).
A successful response reports status 0 and the requested number of completed rounds.

## Independent vector

The [pinned runner reference](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-test/common/src/exam/exb2.rs) specifies this request.
It uses three rounds, seed 1, variant 0, Scratch fill `0xa5`, and Padding fill `0xcc`.

| n | Input | Expected lo, hi | Digest |
| ---: | --- | --- | --- |
| 4 | `abcz\0`, `abdz\0` | `0xffffffff`, 3 | `0x664ce84e` |

Check an embedded NUL, a final compared byte difference, and `n = 0`.
Check the complete response and all preserved bytes.
