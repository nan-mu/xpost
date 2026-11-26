#!/usr/bin/env python3
"""Start the xpost-agent daemon from a freshly built binary.

Why a launcher exists: `cargo run` and a hardcoded `target/.../xpost-agent` both
fail in the same way when `CARGO_TARGET_DIR` is set. `cargo run` works but leaves
no path for anything else to reuse, and a hardcoded path silently runs a stale
binary. This launcher asks `cargo_artifact.py` for the real path, starts the
daemon detached from the terminal, waits for `/health`, and prints the pid.

Usage:
    xpost-agent-run.py                 # bind 0.0.0.0:9240
    xpost-agent-run.py --port 9241
    xpost-agent-run.py --no-build      # reuse the existing artifact
    xpost-agent-run.py --stop

Exit status is non-zero when the daemon does not answer health within the wait,
so a caller can gate on it instead of sleeping and hoping.
"""

from __future__ import annotations

import argparse
import json
import os
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ARTIFACT = ROOT / "scripts" / "cargo_artifact.py"
PID_FILE = ROOT / ".audit" / "xpost-agent.pid"


def artifact_path(no_build: bool) -> Path:
    cmd = [
        sys.executable,
        str(ARTIFACT),
        "--manifest",
        str(ROOT / "xpost-agent" / "Cargo.toml"),
        "--bin",
        "xpost-agent",
    ]
    if no_build:
        cmd.append("--print-only")
    out = subprocess.run(cmd, capture_output=True, text=True, check=True)
    return Path(out.stdout.strip())


def health(port: int, timeout: float = 1.5) -> bool:
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=timeout) as r:
            return json.loads(r.read()).get("ok") is True
    except (urllib.error.URLError, OSError, json.JSONDecodeError):
        return False


def stop() -> int:
    if not PID_FILE.is_file():
        print("no pid file; nothing to stop", file=sys.stderr)
        return 0
    pid = int(PID_FILE.read_text().strip())
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        PID_FILE.unlink(missing_ok=True)
        print(f"pid {pid} already gone", file=sys.stderr)
        return 0
    for _ in range(50):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            PID_FILE.unlink(missing_ok=True)
            print(f"stopped pid {pid}", file=sys.stderr)
            return 0
        time.sleep(0.1)
    print(f"pid {pid} ignored SIGTERM", file=sys.stderr)
    return 1


def main() -> int:
    p = argparse.ArgumentParser(prog="xpost-agent-run")
    p.add_argument("--bind", default="0.0.0.0")
    p.add_argument("--port", type=int, default=9240)
    p.add_argument("--no-build", action="store_true")
    p.add_argument("--stop", action="store_true")
    p.add_argument("--wait-s", type=float, default=20.0)
    args = p.parse_args()

    if args.stop:
        return stop()

    binary = artifact_path(args.no_build)
    if not binary.is_file():
        print(f"{binary} missing; run without --no-build", file=sys.stderr)
        return 1

    log = ROOT / ".audit" / "xpost-agent.log"
    log.parent.mkdir(parents=True, exist_ok=True)
    # A new session detaches the daemon from this shell's process group, so it
    # outlives the command that started it. Redirecting the log keeps stdout from
    # blocking once the pipe's reader is gone.
    with log.open("ab") as fh:
        proc = subprocess.Popen(
            [str(binary), "--bind", f"{args.bind}:{args.port}"],
            stdout=fh,
            stderr=subprocess.STDOUT,
            stdin=subprocess.DEVNULL,
            start_new_session=True,
        )
    PID_FILE.write_text(f"{proc.pid}\n")

    deadline = time.time() + args.wait_s
    while time.time() < deadline:
        if proc.poll() is not None:
            print(f"daemon exited with {proc.returncode}; see {log}", file=sys.stderr)
            return 1
        if health(args.port):
            print(f"pid {proc.pid} binary {binary}", file=sys.stderr)
            print(f"http://{args.bind}:{args.port}/health ok", file=sys.stderr)
            print(proc.pid)
            return 0
        time.sleep(0.2)
    print(f"no health after {args.wait_s}s; see {log}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
