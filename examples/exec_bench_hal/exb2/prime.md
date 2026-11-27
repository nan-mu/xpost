# prime

The EXB2 task ID is 1. The Cargo feature is `prime`.
See the [Rust workload](../src/workloads/prime.rs) and the [original C kernel](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/exb2/prime.h).

## Task

The task counts primes from 2 through `n` and returns the largest prime.
It accepts `n` from 0 through 8192 and uses no Input or Scratch.
The result is the count in `lo` and the maximum in `hi`. Both are zero when the range has no primes.
The C kernel used trial division with the bound `d <= x / d`.
The Rust kernel counts 2 separately. It tests odd candidates using odd divisors with `d * d <= x`.
The maximum divisor and candidate fit in a machine word.
Every measured round computes the count and maximum.

## Agent and runner

Select the `exec-bench-hal-prime` firmware profile.
The firmware accepts task 1 through the [shared EXB2 interface](../README.md).
A successful response reports status 0 and the requested number of completed rounds.

## Independent vector

The [pinned runner reference](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-test/common/src/exam/exb2.rs) specifies this request.
It uses three rounds, seed 1, variant 0, Scratch fill `0xa5`, and Padding fill `0xcc`.

| n | Input | Expected lo, hi | Digest |
| ---: | --- | --- | --- |
| 19 | Empty | 8, 19 | `0x91efb91a` |

Also check an empty range and the upper bound.
Check the complete response and all preserved bytes.
