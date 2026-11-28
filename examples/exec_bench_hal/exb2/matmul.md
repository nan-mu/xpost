# matmul

The EXB2 task ID is 7. The Cargo feature is `matmul`.
See the [Rust workload](../src/workloads/matmul.rs) and the [shared EXB2 interface](../README.md).
The HAL profiles include this workload. AOT, JIT, and rBPF require future release artifacts.

## Task

Multiply two runtime square matrices A and B to produce the full matrix C.

```text
C[i,j] = sum(A[i,k] * B[k,j] for k in 0..n)
```

The dimension `n` is 1 through 26 and variant is 0.
The three matrices use row-major little-endian u32 elements.
Input stores A followed by B and uses `8n²` bytes.
Each input element is 0 through 255.
Scratch stores C and uses `4n²` bytes.
The maximum result element is `26 × 255² = 1,690,650`. It fits in u32.
The maximum request uses `64 + 12 × 26² = 8176` bytes before Padding.
Dimension 27 exceeds the total capacity of 8258 bytes.

The firmware checks dimensions, lengths, and element ranges before computing.
Each round computes all of C from the original A and B. The firmware writes C to independent Scratch.
The result words are `lo = C[0,0]` and `hi = C[n-1,n-1]`.
The shared digest covers every round. The runner checks each output element and all preserved bytes.

## Research and implementation

The October 2, 2026 OpenAI preprint gives an arithmetic upper bound of `O_epsilon(n^(9/4+epsilon))` for each positive epsilon.
Its recursive algorithm uses a fixed base dimension and an exact rank decomposition.
The construction and constants depend on epsilon. [October 2 preprint](https://github.com/openai/math/blob/main/preprints/Matrix-Multiplication-Nine-Fourths-October-2-2026/build/paper.tex)

The November 24, 2026 OpenAI preprint claims a matching lower bound. [November 24 preprint](https://github.com/openai/math/tree/main/preprints/The-matrix-multiplication-exponent-is-nine-fourths-November-24-2026)

Amir Shpilka gives an explicit construction with a base size bounded in terms of epsilon.
These asymptotic results give no measured cost for this ESP32-C3 workload. [ECCC TR26-281](https://eccc.weizmann.ac.il/report/2026/281/)

The implementation uses classical matrix multiplication with `O(n³)` arithmetic.
It accumulates one result row at a time. It walks each row of B in order.
The local array uses at most 26 u32 accumulators. The output resides in Scratch.
A 9/4 construction would require a concrete base case and exact integer implementation for these dimensions.
The reference gives no such base case with bounded MCU resources for `n ≤ 26`.
Strassen's seven-product recursion uses `O(n^log2(7))` arithmetic. Its extra intermediate storage and additions also need measurement at these sizes.

## Agent and runner

Select the `exec-bench-hal-matmul` firmware profile.
The firmware accepts task 7 and records the shared EXB2 tick interval.
Invalid dimensions, variant, task ID, layout, or element values produce a protocol error.
A successful response reports status 0 and the requested number of completed rounds.

## Independent vector

Use `n = 2`, A = [[1, 2], [3, 4]], and B = [[5, 6], [7, 8]].
The matrix definition gives C = [[19, 22], [43, 50]].
Set Input length to 32 bytes and Scratch length to 16 bytes.
Use three rounds, seed 1, variant 0, Scratch fill `0xa5`, and Padding fill `0xcc`.
The expected status is 0 and the completed round count is 3.
The expected result words are 19 and 50. The digest is `0x91eee432`.

Use an independent row-column reference for every element.
Check dimensions 1 and 26, a non-symmetric product, and an identity or zero matrix.
Check rejection of dimensions 0 and 27, invalid lengths, a wrong task ID, and elements above 255.
Check that A, B, Padding, and reserved fields retain their original bytes.
