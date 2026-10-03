#!/usr/bin/env bash
# rust-analyzer flycheck / buildScripts helper for multi-workspace xpost.
#
# Do NOT hardcode a triple here: host crates (xpost-serial / xpost-agent /
# xpost-test) and MCU crates share one overrideCommand, and each directory's
# `.cargo/config.toml` already sets the right `build.target`.
#
# Root workspace: only `-p xpost` (avoids --workspace RV32 false errors on
# host-oriented path deps and keeps the xpost-macros expander).
# Other linked workspaces: plain `cargo check` in that cwd.
set -euo pipefail

msg=(--message-format=json)

if grep -Eq '^name[[:space:]]*=[[:space:]]*"xpost"' Cargo.toml; then
  exec cargo check -p xpost "${msg[@]}" "$@"
fi

exec cargo check "${msg[@]}" "$@"
