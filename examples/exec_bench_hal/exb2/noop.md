# noop

The EXB2 task ID is 0. The Cargo feature is `noop`.
See the [Rust workload](../src/workloads/noop.rs) and the [original C kernel](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/exb2/noop.h).

## Task

The task has no algorithm input or Scratch. It requires `n = 0`.
Each round returns `lo = 0` and `hi = 0`.
The native workload leaves computation to the shared validation, round, and digest path.
The measurement covers that EXB2 entry.

## Agent and runner

Select the `exec-bench-hal-noop` firmware profile.
The firmware accepts task 0 through the [shared EXB2 interface](../README.md).
A successful response reports status 0 and the requested number of completed rounds.

## Independent vector

The [pinned runner reference](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-test/common/src/exam/exb2.rs) specifies this request.
It uses three rounds, seed 1, variant 0, Scratch fill `0xa5`, and Padding fill `0xcc`.

| n | Input | Expected lo, hi | Digest |
| ---: | --- | --- | --- |
| 0 | Empty | 0, 0 | `0x91efd7d5` |

The full response has no Scratch output.
Check the complete response and all preserved bytes.
