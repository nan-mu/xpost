# fletcher32

The EXB2 task ID is 4. The Cargo feature is `fletcher32`.
See the [Rust workload](../src/workloads/fletcher32.rs) and the [original C kernel](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/exb2/fletcher32.h).

## Task

The task computes Fletcher-32 for `n` Input bytes. It accepts 0 through 4096 bytes with no Scratch.
It sums little-endian 16-bit words. Both accumulators start at `0xffff`.
For odd lengths, the final byte forms a word with a zero high byte.
The C and Rust implementations fold each accumulator after 360 full words.
They fold the accumulators again at the end to preserve the original representation.
The result is `lo = (s2 << 16) | s1` and `hi = 0`.

## Agent and runner

Select the `exec-bench-hal-fletcher32` firmware profile.
The firmware accepts task 4 through the [shared EXB2 interface](../README.md).
A successful response reports status 0 and the requested number of completed rounds.

## Independent vector

The [pinned runner reference](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-test/common/src/exam/exb2.rs) specifies this request.
It uses three rounds, seed 1, variant 0, Scratch fill `0xa5`, and Padding fill `0xcc`.

| n | Input | Expected lo, hi | Digest |
| ---: | --- | --- | --- |
| 3 | `01 02 03` (hex) | `0x04050204`, 0 | `0x199d264a` |

Check zero, even, odd, and multi-block lengths.
Check the complete response and all preserved bytes.
