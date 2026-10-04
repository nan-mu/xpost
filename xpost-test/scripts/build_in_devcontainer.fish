#!/usr/bin/env fish
# Build xpost-test for TR3000 (aarch64-unknown-linux-musl) via docker toolchain.
set -l root (dirname (status filename))/..
cd $root
cargo build --release
echo "→ $root/target/aarch64-unknown-linux-musl/release/xpost-test"
