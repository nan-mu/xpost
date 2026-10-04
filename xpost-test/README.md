# xpost-test

TR3000 host harness for formal XDP exams against Mac `xpost-agent`.

## Workspace

| Crate | Path | Role |
|-------|------|------|
| `core` | `core/` | `no_std` ABI lib + host binary `xpost-test` |
| `ebpf` | `ebpf/` | aya TC classifier (`test_bpf`); depends on `core` ABI only |

```bash
# TR3000 release (docker toolchain via rust-toolchain.toml):
cargo build -p core --release --target aarch64-unknown-linux-musl
# or:
fish scripts/build_in_devcontainer.fish
```

Host-only / no bpfel: `TEST_SKIP_EBPF=1 cargo build -p core`.

## Binary layout (`core/src`)

- `agent` — xpost-agent HTTP client (`/v1/*`)
- `db` — formal schema ensure + typed inserts
- `pipeline` — AF_PACKET + TC harness + flash UI
- `exam` — `just_drop` / `just_tx` / `firewall` / empty `dev`
- `config` — compile-time TOML embed

Sole runtime entry: bare `./xpost-test` (no flags). Experiments from `config/default.toml` `run`.

## Profiles

Firmware / XDP flash names live in repo-root `profiles.toml` (`[[fw]]` / `[[xdp]]`). Formal XDP-only: `just-*-cag|t`, `firewall-cag|t`.
