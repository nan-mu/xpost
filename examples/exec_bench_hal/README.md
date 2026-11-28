# Native HAL EXB2

Each Firmware ELF runs one EXB2 workload on an ESP32-C3.
HAL is the native Rust firmware baseline. It uses the board's hardware abstraction layer.
The firmware uses USB Serial/JTAG for control and does not initialize Wi-Fi, XDP, or maps.
Each workload preserves the task semantics of its reference.

The previous firmware linked seven C objects and selected an entry by task ID.
This version selects one Rust module at compile time. The task ID checks the request's identity.
The historical scale sweep and the old C sources remain in Git history.
The [PR #41 snapshot](https://github.com/nan-mu/xpost/tree/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal)
contains the C build, dispatch table, and diagnostic entry.
That build pins C sources from libbpf-mcu commit `07a4ea9a81a7fedf7bef2d6ced8d7fcfcccfd113`.

## Workloads and builds

| Task | Feature and design | Firmware ELF | Agent profile |
| ---: | --- | --- | --- |
| 0 | [noop](exb2/noop.md) | `exec_bench_hal_noop` | `exec-bench-hal-noop` |
| 1 | [prime](exb2/prime.md) | `exec_bench_hal_prime` | `exec-bench-hal-prime` |
| 2 | [log2](exb2/log2.md) | `exec_bench_hal_log2` | `exec-bench-hal-log2` |
| 3 | [copy](exb2/copy.md) | `exec_bench_hal_copy` | `exec-bench-hal-copy` |
| 4 | [fletcher32](exb2/fletcher32.md) | `exec_bench_hal_fletcher32` | `exec-bench-hal-fletcher32` |
| 5 | [bsort](exb2/bsort.md) | `exec_bench_hal_bsort` | `exec-bench-hal-bsort` |
| 6 | [strcmp](exb2/strcmp.md) | `exec_bench_hal_strcmp` | `exec-bench-hal-strcmp` |
| 7 | [matmul](exb2/matmul.md) | `exec_bench_hal_matmul` | `exec-bench-hal-matmul` |

Use the nightly toolchain selected by `rust-toolchain.toml`. The aggregate command requires Cargo 1.91 or newer for `host-tuple`.
Run the commands from `examples/exec_bench_hal`.

```sh
cargo build-hal-all
cargo build-hal --locked --features matmul --bin exec_bench_hal_matmul
```

The first command builds all eight Firmware ELFs in sequence.
The second command builds only `matmul`. Substitute matching entries from the table for another workload.
Cargo requires exactly one workload feature for a Firmware ELF.
The aggregate command exits on the first build failure. Both commands leave the board untouched.

The default paths start at `examples/exec_bench_hal/target/riscv32imc-unknown-none-elf/release/`.
With `CARGO_TARGET_DIR` set, read `target_directory` from `cargo metadata --format-version 1 --no-deps`.
Use that directory before the target and release path components.
Agent profiles explicitly use `--target-dir target` and their matching `fw.artifact` paths.
The release profile uses `opt-level = "s"`, fat LTO, one codegen unit, and debug info level 2.
The target configuration enables frame pointers and stack protection.
The Agent checks the ELF's embedded task ID against the selected profile.

Each bin declares its own `esp_rtos::main` and binds its matching workload type.
The feature selects the workload module. Rust static calls bind the protocol to that type.
The bins share startup, serial transport, and Metrics through the firmware module.
The firmware keeps a reference to the task marker so the linker retains it for Agent preflight.

Run host byte-level checks for each compiled workload. Select the host target for the current machine.

```sh
for workload in noop prime log2 copy fletcher32 bsort strcmp matmul; do
  cargo test --locked --lib --target aarch64-apple-darwin --features "$workload" || exit
done
```

## EXB2 buffer

A request has 8258 bytes. A 64-byte header precedes Input, Scratch, and Padding.
All integers use little-endian order. The selected workload defines the Input and Scratch lengths.
The firmware checks the header, task, reserved fields, dimensions, variant, layout, and round count.
The round count is 1 through 65536. The separate field `n` is the workload parameter.

| Offset | Bytes | Field |
| ---: | ---: | --- |
| 0 | 4 | Magic `0x32425845` (`EXB2`) |
| 4 | 2 | Version `2` |
| 6 | 2 | Header length `64` |
| 8 | 4 | Task ID |
| 12 | 4 | `n` |
| 16 | 4 | Rounds |
| 20 | 4 | Variant (0, or 1 for 64-bit `log2`) |
| 24 | 4 | Seed |
| 28 | 4 | Input length |
| 32 | 4 | Scratch length |
| 36 | 4 | Status |
| 40 | 4 | Completed rounds |
| 44 | 4 | Result low word |
| 48 | 4 | Result high word |
| 52 | 4 | Digest |
| 56, 60 | 4 each | Reserved (0) |

The request sets status to `0xffffffff`.
The HAL firmware returns status 0 on success. The XDP modes return action 2 on success.
Status codes 1 through 7 indicate bad magic, version, task, range, layout, variant, or buffer length.
The response preserves Input, Padding, and reserved bytes.
The low and high result words describe the last complete round.
The firmware writes the result fields after it runs all rounds.
Each workload produces its Scratch output again in every round that writes Scratch.

```text
d = seed XOR 0x811c9dc5
for r in 0..rounds:
    execute one round and obtain lo, hi
    d = (rotl32(d, 5) XOR lo) + (hi XOR r XOR 0x9e3779b9)
```

The digest arithmetic wraps at 32 bits.
The workload pages give independent byte vectors with three rounds, seed 1, and variant 0.
Their requests fill Scratch with `0xa5` and Padding with `0xcc`.

## Agent and runner

Build the Firmware ELFs before preflight. Preflight reads the existing artifacts.
The runner requests the selected profile and records its flashed Firmware ELF SHA-256.
The Agent retains and checks the bytes used for the device write.
Medium and all give each HAL workload its own firmware group and attempt identity.
The runner reuses the firmware for repeats within a group.
On resume, the Agent can check the HAL application and partition table in an exclusive download session.
The HAL EXB2 check skips the contents of the cold XDP partition.

For standalone collection, select `exec_bench` in `xpost-test/profile.toml`.
Set modes and workloads in `xpost-test/config/exec_bench.toml`. An empty workload list selects all supported workloads.
For scoped HAL `matmul` qualification, use these settings in a disposable environment.

```toml
modes = ["hal"]
workloads = ["matmul"]
```

Rebuild the runner after changing its configuration.
HAL uses all eight workloads. AOT, JIT, and rBPF use the seven in the pinned release.
Standalone sampling defaults to `medium`, which runs two single-batch attempts per workload.
For HAL Full, select only `exec_bench` in `profile.toml` and set `sampling = "full"`.
Set `modes = ["hal"]` and `workloads = []` to select all eight workloads.
Full runs five attempts of 100 batches per workload, with three rounds per batch.
Full initializes Protocol V1 in an empty `release` schema.
If the schema has objects, Full checks compatibility before it flashes a Firmware ELF.
Build from `xpost-test` with the compiled revision. The `xpost-test.fish` script sets it.

```sh
XPOST_MEDIUM_REVISION="$(git rev-parse HEAD)" cargo build --release --locked
```

For one recovery attempt, set `retry_run_ids` to the failed Full run IDs.
Keep `workloads = []`. Each retry uses its original repeat number and Firmware ELF.
The runner stores the failed run ID in the new condition's `retry_of` field.

The runner checks the entire response against the independent expected bytes.
The HAL config records the workload and Firmware ELF SHA-256.
The run condition records the Rust baseline, feature, release optimization settings, and timing boundary.
An old Protocol V1 workload constraint can block collection.
The runner reports that constraint and leaves the existing table and rows in place.

The runner calls `POST /v1/exec/batch` with `input_hex`.
The Agent holds its device lock through reset, upload, invoke, and read.
The firmware reports `OK init` when the serial handler is ready.
The runner waits for this marker. It sends no wire request for the marker.

| Serial command | Behavior |
| --- | --- |
| `exec batch reset` | Start a new upload at byte zero. |
| `exec batch write offset hex` | Append in order, with at most 256 bytes per block. |
| `exec batch invoke` | Execute one complete upload and return the status and DUT ticks. |
| `exec batch read offset max_bytes` | Read at most 512 result bytes at the offset. |
| `perf prepare uuid metrics profile` | Open an attributed Metrics session. |
| `perf arm epoch` | Record the initial counters and tick. |
| `perf freeze epoch` | Freeze the final counters and tick. |
| `perf read epoch offset max_bytes` | Read part of the frozen Metrics data. |
| `perf release epoch` | Release the Metrics session. |

The HAL tick interval starts before the Rust EXB2 entry and ends after response publication.
It includes validation, all rounds, and their digest updates.
Serial transfers, Metrics counters, and export are outside that interval.
The clock measures integer microseconds since the device started.
The XDP carriers time the XPost invocation path.
Use matching work, input, and timing boundaries when interpreting measurements.

The transport uses 52 serial requests per batch. Upload and read use 33 and 17 blocks, respectively.
Historical upload and read timeouts remain under investigation.
A read failure does not establish whether invoke ran.
The recorded timeout hypothesis concerns reply timestamps that can predate the UART write marker.
The source does not establish that this caused a historical failure.
See [UART timestamp capture](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-serial/src/client/port/uart.rs#L144-L155)
and [reply selection](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-serial/src/client/record/mod.rs#L261-L266).
Normal commands use a 5-second deadline. Invoke uses a 30-second deadline.
