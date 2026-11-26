#!/usr/bin/env fish
# Build xpost-test for TR3000 (aarch64-unknown-linux-musl) via docker toolchain.
set -l root (path resolve (status dirname)/..)
cd "$root"; or exit $status
set -lx XPOST_MEDIUM_REVISION (git rev-parse HEAD); or exit $status
cargo build --release; or exit $status
echo "→ $root/target/aarch64-unknown-linux-musl/release/xpost-test"
