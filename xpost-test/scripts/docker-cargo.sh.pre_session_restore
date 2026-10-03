#!/usr/bin/env bash
# Forward cargo invocations into quirky_villani (mount: /Users/nan/bs:/Users/nan/bs).
set -euo pipefail

CONTAINER="${XPOST_TEST_DOCKER:-quirky_villani}"
HOST_ROOT="${XPOST_HOST_ROOT:-/Users/nan/bs}"
cwd="$(pwd -P)"

case "$cwd" in
  "$HOST_ROOT"|"$HOST_ROOT"/*) workdir="$cwd" ;;
  *)
    echo "docker-cargo: cwd $cwd is outside $HOST_ROOT" >&2
    exit 1
    ;;
esac

if ! docker inspect -f '{{.State.Running}}' "$CONTAINER" 2>/dev/null | grep -q true; then
  echo "docker-cargo: container $CONTAINER is not running" >&2
  exit 1
fi

tty_flags=(-i)
if [ -t 1 ]; then
  tty_flags=(-it)
fi

exec docker exec "${tty_flags[@]}" -w "$workdir" "$CONTAINER" cargo "$@"
