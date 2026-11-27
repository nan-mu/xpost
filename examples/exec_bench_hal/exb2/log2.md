# log2

The EXB2 task ID is 2. The Cargo feature is `log2`.
See the [Rust workload](../src/workloads/log2.rs) and the [original C kernel](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/exb2/log2_int.h).

## Task

The task computes the integer floor of `log2(x)` for each element. It defines `log2(0) = 0`.
It accepts `n` from 0 through 1024 with no Scratch.
Variant 0 uses `n` little-endian `u32` values. Variant 1 uses `n` little-endian `u64` values.
The native implementation uses integer bit positions. It reads the two halves of each `u64` separately.
The result `lo` is the sum of the bit positions.
The result `hi` starts at zero and updates as `h = rotl32(h, 5) XOR (r_i + i)` for each index `i`.
The EXB2 digest uses both result words.

## Agent and runner

Select the `exec-bench-hal-log2` firmware profile.
The firmware accepts task 2 through the [shared EXB2 interface](../README.md).
A successful response reports status 0 and the requested number of completed rounds.

## Independent vector

The [pinned runner reference](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-test/common/src/exam/exb2.rs) specifies this request.
It uses three rounds, seed 1, variant 0, Scratch fill `0xa5`, and Padding fill `0xcc`.

| n | Input | Expected lo, hi | Digest |
| ---: | --- | --- | --- |
| 4 | u32 [0, 1, 4, 255] | 9, `0x48a` | `0x91fe55a4` |

Also check zero and bit positions across the 32-bit boundary in variant 1.
Check the complete response and all preserved bytes.
