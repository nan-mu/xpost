#!/usr/bin/env bash
# Mac-side client for TR3000 `./test serve` (single `test` binary on router).
set -euo pipefail

TEST_HTTP="${TEST_HTTP:-http://192.168.110.142:19081}"
TEST_SSH="${TEST_SSH:-tr3000}"
AGENT_ENDPOINT="${XPOST_AGENT_ENDPOINT:-192.168.110.96:9240}"
ENSURE_AGENT="${ENSURE_AGENT:-1}"
XPOST_ROOT="${XPOST_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}"

usage() {
  cat <<'EOF'
Usage: fwctl.sh [--no-agent] <bench> [--smoke] [--formal]

Submit a bench job to TR3000 `./test serve` and stream captured stdout/stderr.

Env:
  TEST_HTTP              Base URL (default http://192.168.110.142:19081)
  TEST_SSH               SSH host when direct HTTP fails (default tr3000)
  XPOST_AGENT_ENDPOINT   Mac flash agent (default from config)
  ENSURE_AGENT           1=start agent if down (default), 0=skip
  XPOST_ROOT             xpost workspace for `cargo xtask agent`

Examples:
  ./scripts/fwctl.sh all --smoke --formal
  ./scripts/fwctl.sh drop-xdp --smoke
EOF
}

http_health() {
  local base="$1"
  case "$base" in
    http://*|https://*) ;;
    *) base="http://$base" ;;
  esac
  curl -fsS --max-time 2 "${base%/}/health" >/dev/null
}

ensure_agent() {
  if http_health "$AGENT_ENDPOINT"; then
    echo "agent reachable at $AGENT_ENDPOINT" >&2
    return 0
  fi
  local port="${AGENT_ENDPOINT##*:}"
  port="${port%%/*}"
  local listen="0.0.0.0:${port}"
  echo "starting xpost agent on $listen …" >&2
  (cd "$XPOST_ROOT" && cargo xtask agent --listen "$listen" --no-monitor) &
  for _ in $(seq 1 180); do
    sleep 1
    if http_health "$AGENT_ENDPOINT"; then
      echo "agent ready at $AGENT_ENDPOINT" >&2
      return 0
    fi
  done
  echo "agent at $AGENT_ENDPOINT did not become ready" >&2
  return 1
}

bench=""
smoke=false
formal=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --no-agent) ENSURE_AGENT=0; shift ;;
    --smoke) smoke=true; shift ;;
    --formal) formal=true; shift ;;
    -*) echo "unknown flag: $1" >&2; usage; exit 2 ;;
    *)
      if [[ -z "$bench" ]]; then bench="$1"; else echo "unexpected arg: $1" >&2; exit 2; fi
      shift
      ;;
  esac
done

[[ -n "$bench" ]] || { usage; exit 2; }

command -v curl >/dev/null || { echo "curl required" >&2; exit 1; }
command -v jq >/dev/null || { echo "jq required" >&2; exit 1; }

if [[ "$ENSURE_AGENT" == "1" ]]; then
  ensure_agent
fi

via_ssh=false
base="${TEST_HTTP%/}"
if ! curl -fsS --connect-timeout 2 "${base}/healthz" >/dev/null 2>&1; then
  via_ssh=true
  base="http://127.0.0.1:19081"
  echo "fwctl: direct ${TEST_HTTP} unreachable, using ssh ${TEST_SSH}" >&2
fi

body=$(jq -nc --arg bench "$bench" --argjson smoke "$smoke" --argjson formal "$formal" \
  '{bench:$bench, smoke:$smoke, formal:$formal}')

api_get() {
  local path=$1
  if $via_ssh; then
    ssh "$TEST_SSH" "curl -fsS '${base}${path}'"
  else
    curl -fsS "${base}${path}"
  fi
}

api_post_json() {
  local path=$1 json=$2
  if $via_ssh; then
    ssh "$TEST_SSH" "curl -fsS -X POST '${base}${path}' -H 'Content-Type: application/json' -d '$json'"
  else
    curl -fsS -X POST "${base}${path}" -H 'Content-Type: application/json' -d "$json"
  fi
}

job_id=$(api_post_json /api/jobs "$body" | jq -r '.job_id')

echo "job $job_id accepted bench=$bench" >&2

log_off=0
while true; do
  job=$(api_get "/api/jobs/$job_id")
  log_bytes=$(echo "$job" | jq -r '.log_bytes')
  terminal=$(echo "$job" | jq -r '.terminal')

  while [[ "$log_off" -lt "$log_bytes" ]]; do
    chunk=$(api_get "/api/jobs/$job_id/log?offset=$log_off")
    text=$(echo "$chunk" | jq -r '.chunk')
    next=$(echo "$chunk" | jq -r '.next_offset')
    eof=$(echo "$chunk" | jq -r '.eof')
    if [[ -n "$text" && "$text" != "null" ]]; then
      printf '%s' "$text"
    fi
    log_off=$next
    if [[ "$eof" == "true" ]]; then break; fi
  done

  if [[ "$terminal" == "true" ]]; then
    state=$(echo "$job" | jq -r '.state')
    message=$(echo "$job" | jq -r '.message')
    echo "" >&2
    echo "job $job_id finished: state=$state message=$message" >&2
    [[ "$state" == "4" ]] || exit 1
    exit 0
  fi
  sleep 0.5
done
