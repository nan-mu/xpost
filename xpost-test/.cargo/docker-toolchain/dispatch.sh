#!/bin/sh
# rustup rejects a relative `path` in rust-toolchain.toml, so the committed file
# names a machine-level directory. install.sh copies this file there.
#
# The checkout that contains the cwd owns the build, so this runs that
# checkout's shim rather than a copy of it.
set -eu

tool="$(basename "$0")"
root="$(git -C "$(pwd -P)" rev-parse --show-toplevel 2>/dev/null)" || {
  echo "xpost-test docker-toolchain: $(pwd -P) is not inside a git checkout" >&2
  exit 1
}
shim="${root}/xpost-test/.cargo/docker-toolchain/bin/${tool}"
if [ ! -x "$shim" ]; then
  echo "xpost-test docker-toolchain: no executable ${tool} shim at ${shim}" >&2
  exit 1
fi
exec "$shim" "$@"
