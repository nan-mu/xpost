# XDP deployment design

## Ownership and identities

| Component | Responsibility |
| --- | --- |
| libbpf-mcu | Publish XDP ELFs and their v3 program indexes |
| xpost-agent | Fetch the release and bind the indexed deployment to a Firmware ELF |
| xpost-elf | Decode XPM1 and check and encode XDP ELFs |
| xpost-serial | Check the device firmware and partition table before each cold install |
| XPOST runtime | Check the image, preload the slot, and publish the entry |
| xpost-test | Schedule experiments and store and analyze observations |

An XDP profile names a local image, a producer program, a mode, and a target slot.
The default target slot is 0. Use slot 1 for a prepared idle image.
The Agent keeps names in the producer index separate from local names.
HAL profiles use firmware only.
AOT and JIT use the native entry. rBPF uses the interpreter.
Metrics and Trace are collection variants.

## Agent configuration

The Agent searches upward from its current directory for `profiles.toml`.
Use `XPOST_ROOT` to select a workspace. Use `XPOST_PROFILES` to select its profile file.
The Mac uses authenticated `gh` for release downloads and the system `tar` for 7z members.
On Linux, the Agent uses `bsdtar`.

```toml
xdp_release = "v0.1.1"

[[xdp]]
name = "just_tx"
fw = "just-tx-metrics"
program = "just_tx"
mode = "aot"
```

This selects `llvm-aot` from `just_tx.7z`.
For JIT the index names `jit`. For rBPF it names `ebpf`.
Use `fw.artifact` for the linked Firmware ELF and `fw.partition_table` for its CSV.
For exec, `fw.xdp_image_capacity` sets the limit for the complete cold XDP ELF.
The SRAM slot has its own payload size limit.
The two exec firmware profiles use different target directories.
Their Cargo features select different engines.

## Validation and writes

The Agent checks the archive SHA-256 before parsing its index.
It caches archives by their verified digest in `.cache/xpost-agent/releases/`.
It publishes cache entries atomically and checks the digest on every read.
The Agent checks source, BPF object, mode, indexed deployment, and embedded program name.
A cache entry from another release or program cannot replace a missing deployment.

The Agent reads the target slot and helper from the linked Firmware ELF.
XPM1, slot sections, the helper symbol, and the selected partition describe the target layout.
The Agent checks the slot capacity and the cold storage capacity before a write.
For a native deployment at the same address, it can reuse the published payload.
For a local alias, it changes the container name and retains the payload and maps.
For rBPF it can also change the slot metadata. The interpreter resolves map indexes at load.
The pinned native release archives lack relocation records.
Case AOT can use slot B when its payload digest and the actual layout match the direct-link evidence.
For an unmatched native slot or helper, preparation fails with both addresses.

The prepared image records the release and final container digests.
It binds the final bytes to the firmware digest, mode, helper, and target slot.
Record the release digest and prepared container digest separately.
The serial worker receives the retained bytes.
The Agent resolves the flash span from the retained partition table bytes.
Firmware co-flash prepares all linked XDP ELFs before it writes firmware or clears the cold table.
The Agent keeps its device lock through firmware flash, clear, and linked installs.
Standalone install checks the application segment and partition table on each attempt.
The check and write share an exclusive device session.
A planned install may replace the only image in a cold table.
That operation rejects a table with multiple images before erase.
The runner supplies the preflight Firmware ELF digest for each planned write.
The Agent compares that digest with the retained Firmware ELF before any write.

Use the slot form of `flash_xdp` to preload an installed image.
The runtime verifies the image before it changes the slot state.
A preload does not publish an entry.
Use `xdp switch` to publish a staged slot after its generation check.
The runtime retains the current entry when preload fails.

## Entry points and measurements

The Agent shares operations between HTTP and MCP.
The CLI `preflight` command checks the release inventory and existing firmware layouts without USB.
The `preflight_medium` MCP tool and `GET /v1/preflight/medium` use the same checks.
A successful preflight establishes host preparation only.

Place each experimental rate and repeat inside its firmware group.
Count firmware flash, cold install, SRAM preload, and entry publish separately.
Keep host and DUT clocks separate.
Document the stimulus, timing boundaries, and metric denominators in each collector module.
Use the run ID to trace inputs, images, measurements, and the result query.
Check all callers before removing a legacy execution path.
Retain historical records while retiring their writers.
Report missing or unqualified observations with their recorded causes.
