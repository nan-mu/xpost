#!/usr/bin/env bash
# Optional explicit entry; same effect as `cargo build` via docker-toolchain.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
cargo build -p test --release --target aarch64-unknown-linux-musl
echo "→ $ROOT/target/aarch64-unknown-linux-musl/release/probe"
