#!/usr/bin/env bash
# Install the machine-level dispatcher that rust-toolchain.toml names.
# Rerun after dispatch.sh changes; the end state is the same.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd -P)"
toolchain_path="$(sed -n 's/^path = "\(.*\)"$/\1/p' "$here/../../rust-toolchain.toml")"
case "$toolchain_path" in
  /*) ;;
  *)
    echo "install-docker-toolchain: rust-toolchain.toml has no absolute path" >&2
    exit 1
    ;;
esac

bin="$toolchain_path/bin"
mkdir -p "$bin"
tmp="$(mktemp "$bin/.cargo.XXXXXX")"
trap 'rm -f "$tmp"' EXIT
cp "$here/dispatch.sh" "$tmp"
chmod 755 "$tmp"
mv -f "$tmp" "$bin/cargo"
for tool in rustc rustdoc rustfmt clippy-driver; do
  ln -sfn cargo "$bin/$tool"
done
printf 'installed docker-toolchain dispatcher at %s\n' "$toolchain_path"
