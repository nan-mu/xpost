# xpost

## Project Architecture

`xpost` provides an XDP runtime for Rust MCU applications.
The target is ESP32-C3 with RV32IMC.

- The root crate owns runtime loading, maps, helpers, and ingress execution.
- `esp-radio` owns the radio driver and its ingress hooks.
- `xpost-type` owns shared actions and traits.
- `xpost-elf` owns XDP ELF encoding and the XPM1 memory contract.
- `xpost-serial` owns the wire protocol and serial transport.
- `xpost-agent` owns firmware builds, device flashing, and serial control.
- `xpost-test` owns experiment procedures, measurements, and database records.
- `libbpf-mcu` produces XDP artifacts and their reports.
- `xpost-artifact` reads, validates, caches, and resolves those artifacts.

The host tools and firmware examples have separate Cargo workspaces.
Read each workspace's Cargo configuration before building it.
Use `xpost-type` for current traits.

Read `docs/user-decisions.md` before work and before decisions that it covers.
If new facts contradict a recorded final decision, ask the user before proceeding.
Append new final decisions with the date and reason.
Preserve earlier entries when a later decision supersedes them.
Do not record ordinary code facts as user decisions.

## Artifact Contract

`libbpf-mcu` is the only producer of XDP artifacts.
`xpost-artifact` is the only entry for artifact parsing and validation.
Do not duplicate the upstream manifest or report schema in XPOST.
Do not build XDP from `xpost-agent` or `xpost-test`.
Firmware profiles retain their Cargo build commands.
XDP profiles contain names, firmware links, programs, modes, and optional targets.
Reject obsolete XDP build commands in configuration.

`profiles.toml` sets `build_root = "../build"` for the producer's default output directory.
Resolve containers through the index, program manifest, and mode report.
Do not infer artifact paths from filenames.
Use the upstream contract pinned by `xpost-artifact::CONTRACT_COMMIT`.

| XPOST mode | Producer mode | Execution path |
| --- | --- | --- |
| `aot` | `rv32` | C source compiled directly to RV32IMC |
| `jit` | `jit` | eBPF bytecode compiled to RV32IMC |
| `rbpf` | `bpf-xpost` | eBPF interpreter |
| `hal` | None | Native firmware without an XDP container |

Only one verified primary `xpost_container` can supply a flash image.
Raw BPF objects and debug ELFs are not flash containers.
The report's firmware SHA-256 must match the firmware ELF used for the operation.
Standalone XDP installation uses the existing linked firmware ELF.
It does not build firmware or XDP.
Validate the device application and partition table against that firmware before cold installation.
Keep validation and installation in one exclusive download session.
Reject unknown device identity, read failures, and mismatches before XDP writes.
Validate firmware on every cold installation attempt.
Do not use process memory as device identity proof.

The local cache is `.cache/xpost-artifact/`.
It stores verified, unchanged container bytes and source metadata.
Separate entries by program, mode, profile name, and artifact SHA-256.
Publish cache files atomically.
Validate cached identity, size, embedded name, firmware SHA-256, and artifact SHA-256 before use.
Do not track cache contents in Git.

Read and validate upstream metadata before cache use.
Validate report program and producer mode against the request.
Validate every reported input artifact's role, path, and SHA-256.
Resolve input paths within the current program directory, including valid references to sibling mode directories.
Require cached `source_id` and `input_set_id` to match the current report for sibling profile reuse.
Do not require matching `configuration_id` for sibling profile reuse.
A valid source with another embedded profile name can use a matching cached profile.
Missing target profiles require generation through `libbpf-mcu`.
Failed or stale upstream reports cannot use cached replacements.
Hash mismatches and corrupt caches must fail.

Complete all linked XDP validation and cache preparation before the first device write.
Then flash firmware, clear the cold table, and install linked containers in order.
HAL profiles skip XDP resolution and installation.
Validate real input paths against their allowed roots.
Reject symlink escapes and invalid program or profile path segments.
Keep the canonical cache root and its directory components inside the canonical XPOST workspace.

## Agent Operations

Direct device operations must use `xpost-agent` MCP tools.
Read each tool's description and input schema before calling it.
Validate each returned result.
If MCP is unavailable, stop the affected device operation.
Do not substitute HTTP requests or direct serial access.
Do not start the daemon automatically.
These rules include cold-table operations.
The `xpost-test` program may use its existing HTTP client internally.

## Runtime Invariants

Use bare-metal firmware without ESP-IDF.
Preserve XPM1 and the board's partition layout.
Load container image bytes into slots without parsing the firmware ELF on the device.
Keep the fixed two-slot design.
Do not add a third resident image slot.
HAL firmware must not initialize the XDP runtime or map runtime.

Keep map storage and helper dispatch in the runtime.
Treat interpreter map pointers as helper results, not interpreter-owned maps.
Read `rbpf/README.md` before changing interpreter helper, map, memory, or stack behavior.
Preserve the MCU interpreter's allocator-free design.
Preserve its verifier, instruction budget, and per-access permission validation.
Do not add eBPF-to-eBPF calls or an RV32 JIT to that interpreter.
Keep control-plane errors separate from helper errno values.

## Testing

The root workspace defaults to `riscv32imc-unknown-none-elf`.
Use an explicit host target for host crate tests.
Select the host target for the current machine.
The following commands use `aarch64-apple-darwin` as a Mac example.

```sh
cargo test --manifest-path xpost-artifact/Cargo.toml --target aarch64-apple-darwin
cargo test --manifest-path xpost-agent/Cargo.toml --target aarch64-apple-darwin
cargo test --manifest-path xpost-serial/Cargo.toml --target aarch64-apple-darwin --features dev,client,server
bash xpost-test/docker-toolchain-resolve.sh
```

Install the Docker toolchain dispatcher before the first `xpost-test` build:

```sh
bash xpost-test/.cargo/docker-toolchain/install.sh
```

Repeat this command after dispatcher changes.
Each worktree uses its own shim through the shared dispatcher.
Do not copy the toolchain configuration between worktrees.

Validate formatting with rustfmt for changed Rust workspaces.
Use meaningful tests for validation failures and device write ordering.
Do not report device tests as passed unless they ran.

Use the formal runner when the user requests a system experiment.
Run this script from the repository root:

```sh
./xpost-test/scripts/xpost-test.fish
```

On TR3000, formal execution is `./xpost-test` without arguments or added environment variables.
Change experiment selection through configuration and source.
Rebuild and upload before a new run.
Do not use `TEST_SKIP_EBPF` for TR3000 runs.
Do not execute `DROP` or `TRUNCATE` without an explicit database-clear request.
Read test stdout and stderr from the terminal.
Do not redirect test output to temporary files or process it with `tee` or `tail`.
Use current source and configuration for experiment rates, repetitions, metrics, and database fields.

## Documentation

Use **Firmware ELF** for the board application.
Use **XDP ELF** for the installable container.
Preserve executable names such as `xpost-compact-elf`.

Apply `.agents/skills/asd-ste100/SKILL.md` to English agent instructions.
Apply `.agents/skills/ste-zh/SKILL.md` to Chinese technical text.
Use one action per instruction and one term per concept.
Preserve conditions, uncertainty, numbers, units, and machine-readable identifiers.
Keep English instructions within 20 words and descriptions within 25 words where precision permits.
Keep Chinese instructions within 25 characters and descriptions within 40 characters where precision permits.
Delete vague wording, empty phrases, and marketing claims.
Run the corresponding `scripts/ste-lint.py` or `scripts/ste-lint-zh.py` with Python.
Use `python3` when `python` is unavailable.
Review findings manually.
Preserve identifiers when the linter mistakes them for prose.

Explain experiments before interpreting results.
Define network and MCU terms before use.
Compare timing boundaries before comparing measurements.
Do not report speedups from incompatible intervals or empty samples.
Use `analysis/` for current analysis.
Do not restore archived benchmark packages or run their obsolete figure tasks.
