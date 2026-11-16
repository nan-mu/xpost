#!/usr/bin/env bash
# Resolve hook for the docker-toolchain cargo shim. Does not start Docker.
set -euo pipefail

SHIM="$(cd "$(dirname "$0")" && pwd -P)/.cargo/docker-toolchain/bin/cargo"
HARDCODED="/Users/nan/bs/xpost/xpost-test"
WORKTREE_PROJECT="$(cd "$(dirname "$SHIM")/../../.." && pwd -P)"

if [ ! -f "$SHIM" ]; then
  echo "missing shim: $SHIM" >&2
  exit 1
fi

tmp="$(mktemp -d)"
vol=""
cleanup() {
  rm -rf "$tmp"
  if [ -n "$vol" ]; then
    rm -rf "$vol"
  fi
}
trap cleanup EXIT

parent_a="$(mktemp -d "$tmp/parent-a.XXXXXX")"
parent_b="$(mktemp -d "$tmp/parent-b.XXXXXX")"
mkdir -p "$parent_a/xpost-test/nested" "$parent_a/xpost-test/inner/xpost-test" "$parent_b/xpost-test"
printf '[package]\nname = "a"\nversion = "0.0.0"\n' >"$parent_a/xpost-test/Cargo.toml"
printf '[package]\nname = "b"\nversion = "0.0.0"\n' >"$parent_b/xpost-test/Cargo.toml"

proj_a="$(cd "$parent_a/xpost-test" && pwd -P)"
proj_b="$(cd "$parent_b/xpost-test" && pwd -P)"
stage_parent="$(mktemp -d "$tmp/stage.XXXXXX")"

fail() {
  echo "FAIL: $*" >&2
  if [ -s "$tmp/err" ]; then
    echo "stderr:" >&2
    cat "$tmp/err" >&2
  fi
  if [ -n "${INVOKE_OUT:-}" ]; then
    echo "stdout:" >&2
    printf '%s\n' "$INVOKE_OUT" >&2
  fi
  exit 1
}

invoke() {
  local cwd="$1"
  shift
  INVOKE_STATUS=0
  INVOKE_OUT="$(
    cd "$cwd" &&
      env -u XPOST_TEST_IN_DOCKER -u XPOST_HOST_ROOT "$@" bash "$SHIM" 2>"$tmp/err"
  )" || INVOKE_STATUS=$?
}

expect_resolve() {
  local cwd="$1"
  local want_project="$2"
  shift 2
  invoke "$cwd" XPOST_TEST_DOCKER_RESOLVE=1 "XPOST_TEST_DOCKER_STAGE=$stage_parent" "$@"
  if [ "$INVOKE_STATUS" -ne 0 ]; then
    fail "resolve from $cwd exited $INVOKE_STATUS"
  fi
  printf '%s\n' "$INVOKE_OUT" >"$tmp/out"
  local lines
  lines="$(wc -l <"$tmp/out" | tr -d ' ')"
  if [ "$lines" -ne 2 ]; then
    fail "resolve from $cwd printed $lines lines, want 2"
  fi
  GOT_PROJECT="$(sed -n '1p' "$tmp/out")"
  GOT_STAGE="$(sed -n '2p' "$tmp/out")"
  if [ "$GOT_PROJECT" != "project=$want_project" ]; then
    fail "project line [$GOT_PROJECT] != project=$want_project"
  fi
  if [ "$GOT_PROJECT" = "project=$HARDCODED" ]; then
    fail "project line is the hardcoded checkout"
  fi
  case "$GOT_STAGE" in
    "stage=$stage_parent"/*) ;;
    *) fail "stage [$GOT_STAGE] is not under $stage_parent" ;;
  esac
}

expect_fail() {
  local cwd="$1"
  shift
  invoke "$cwd" "$@"
  if [ "$INVOKE_STATUS" -eq 0 ]; then
    fail "expected nonzero from $cwd"
  fi
}

expect_resolve "$proj_a" "$proj_a"
stage_a="$GOT_STAGE"
expect_resolve "$proj_b" "$proj_b"
stage_b="$GOT_STAGE"
if [ "$stage_a" = "$stage_b" ]; then
  fail "two checkouts share a stage: $stage_a"
fi

expect_resolve "$proj_a/nested" "$proj_a"
if [ "$GOT_STAGE" != "$stage_a" ]; then
  fail "subdirectory stage [$GOT_STAGE] != $stage_a"
fi

expect_resolve "$parent_a/xpost-test/inner/xpost-test" "$proj_a"
if [ "$GOT_STAGE" != "$stage_a" ]; then
  fail "nearer xpost-test without Cargo.toml hid $proj_a"
fi

expect_resolve "$WORKTREE_PROJECT" "$WORKTREE_PROJECT"

plain="$(mktemp -d "$tmp/plain.XXXXXX")"
mkdir -p "$plain/xpost-test/sub"
expect_fail "$plain" XPOST_TEST_DOCKER_RESOLVE=1 "XPOST_TEST_DOCKER_STAGE=$stage_parent"
expect_fail "$plain/xpost-test" XPOST_TEST_DOCKER_RESOLVE=1 "XPOST_TEST_DOCKER_STAGE=$stage_parent"
expect_fail "$plain/xpost-test/sub" XPOST_TEST_DOCKER_RESOLVE=1 "XPOST_TEST_DOCKER_STAGE=$stage_parent"

expect_fail "$proj_b" XPOST_TEST_DOCKER_RESOLVE=1 "XPOST_TEST_DOCKER_STAGE=$stage_parent" "XPOST_HOST_ROOT=$parent_a"
expect_resolve "$proj_a" "$proj_a" "XPOST_HOST_ROOT=$parent_a"
if [ "$GOT_STAGE" != "$stage_a" ]; then
  fail "host-root allow did not keep stage $stage_a"
fi

if [ -d /Users/nan/bs ]; then
  expect_fail "$WORKTREE_PROJECT" XPOST_TEST_DOCKER_RESOLVE=1 "XPOST_TEST_DOCKER_STAGE=$stage_parent" XPOST_HOST_ROOT=/Users/nan/bs
fi

vol="$(mktemp -d /Volumes/SSD/xpost-docker-stage.XXXXXX)"
invoke "$proj_a" XPOST_TEST_DOCKER_RESOLVE=1 "XPOST_TEST_DOCKER_STAGE=$vol"
if [ "$INVOKE_STATUS" -eq 0 ]; then
  fail "stage parent under /Volumes exited 0"
fi
case "$INVOKE_OUT" in
  *project=*) fail "stage parent under /Volumes still printed a project" ;;
esac

if [ -n "$(find "$stage_parent" -mindepth 1 -print -quit)" ]; then
  fail "resolve wrote into $stage_parent"
fi

echo "ok"
