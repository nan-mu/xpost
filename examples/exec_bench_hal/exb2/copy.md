# copy

The EXB2 task ID is 3. The Cargo feature is `copy`.
See the [Rust workload](../src/workloads/copy.rs) and the [original C kernel](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/exb2/copy.h).

## Task

The task copies `n` bytes from Input to independent Scratch.
It accepts `n` from 0 through 4096. Both lengths equal `n`.
The C kernel copied one byte at a time. Rust uses a slice copy and writes Scratch in each round.
The result is `lo = n` and `hi = 0`.

## Agent and runner

Select the `exec-bench-hal-copy` firmware profile.
The firmware accepts task 3 through the [shared EXB2 interface](../README.md).
A successful response reports status 0 and the requested number of completed rounds.

## Independent vector

The [pinned runner reference](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-test/common/src/exam/exb2.rs) specifies this request.
It uses three rounds, seed 1, variant 0, Scratch fill `0xa5`, and Padding fill `0xcc`.

| n | Input | Expected lo, hi | Digest |
| ---: | --- | --- | --- |
| 4 | `44 20 82 3c` (hex) | 4, 0 | `0x91efe859` |

The expected Scratch bytes are `44 20 82 3c` (hex). Check Input and Padding for preservation.
Check the complete response and all preserved bytes.
