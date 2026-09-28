#!/usr/bin/env bash
# rust-analyzer flycheck / buildScripts helper for multi-workspace xpost.
#
# Root workspace: only `-p xpost` (avoids --workspace/--all-targets RV32
# false errors and keeps the host xpost-macros expander).
# Linked firmware workspaces (e.g. examples/just_drop): check the local
# package so build.rs / mem_map codegen run.
set -euo pipefail

target=riscv32imc-unknown-none-elf
msg=(--message-format=json)

if grep -Eq '^name[[:space:]]*=[[:space:]]*"xpost"' Cargo.toml; then
  exec cargo check -p xpost --target "$target" "${msg[@]}" "$@"
fi

exec cargo check --target "$target" "${msg[@]}" "$@"
