# bsort

The EXB2 task ID is 5. The Cargo feature is `bsort`.
See the [Rust workload](../src/workloads/bsort.rs) and the [original C kernel](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/exb2/bsort.h).

## Task

The task sorts `n` little-endian `u32` values in nondecreasing order. It accepts 0 through 128 elements.
Input and Scratch each use `4n` bytes.
Each round initializes a local array from Input and sorts it with Rust's `sort_unstable`.
It writes all sorted elements to Scratch and preserves Input.
The result is `lo` for the minimum and `hi` for the maximum. Both are zero when `n = 0`.
The original C kernel used bubble sort. The Rust result supports a task-level comparison across sorting algorithms.

## Agent and runner

Select the `exec-bench-hal-bsort` firmware profile.
The firmware accepts task 5 through the [shared EXB2 interface](../README.md).
A successful response reports status 0 and the requested number of completed rounds.

## Independent vector

The [pinned runner reference](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-test/common/src/exam/exb2.rs) specifies this request.
It uses three rounds, seed 1, variant 0, Scratch fill `0xa5`, and Padding fill `0xcc`.

| n | Input | Expected lo, hi | Digest |
| ---: | --- | --- | --- |
| 4 | u32 [8, 2, 1, 8] | 1, 8 | `0x91efbaee` |

The expected Scratch is little-endian u32 [1, 2, 8, 8]. Check duplicates and unsigned values.
Check the complete response and all preserved bytes.
