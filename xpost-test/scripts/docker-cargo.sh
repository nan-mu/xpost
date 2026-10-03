#!/usr/bin/env bash
# Forward cargo into quirky_villani via the path-toolchain shim semantics.
# Prefer: cd xpost-test && cargo … (rust-toolchain.toml → docker-toolchain).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
exec "$ROOT/.cargo/docker-toolchain/bin/cargo" "$@"
