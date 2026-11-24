"""Black-box lifecycle tests for binaries built with --features test-device."""

from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


binary = Path(sys.argv[1]).resolve()
alternate = Path(sys.argv[2]).resolve() if len(sys.argv) > 2 else None
repository = Path(__file__).resolve().parents[2]


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def run(root, args, *, check=True, executable=binary, env=None):
    result = subprocess.run(
        [str(executable), *args], cwd=root, env=env, text=True,
        capture_output=True, timeout=30,
    )
    if check:
        assert result.returncode == 0, (args, result.stdout, result.stderr)
    return result


def fails(root, args, message, **kwargs):
    result = run(root, args, check=False, **kwargs)
    assert result.returncode != 0 and message in result.stderr, result
    return result


def status(root, env, **kwargs):
    return json.loads(run(root, ["status"], env=env, **kwargs).stdout)


def http(endpoint, path, body=None):
    if body is None:
        request = endpoint + path
    else:
        request = Request(
            endpoint + path, json.dumps(body).encode(),
            headers={"Content-Type": "application/json"},
        )
    with urlopen(request, timeout=2) as reply:
        return json.load(reply)


def compare_health(instance):
    assert http(instance["endpoint"], "/health") == {"ok": True, "message": "ok"}


def expect_closed(endpoint):
    try:
        http(endpoint, "/health")
    except URLError:
        return
    raise AssertionError(f"daemon is still responding at {endpoint}")


def eventually(predicate):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.025)
    raise AssertionError("timed out waiting for fixture")


def workspace(path):
    path.mkdir()
    (path / "profiles.toml").write_text((repository / "profiles.toml").read_text())
    return path


def start_args(port, *extra):
    return ["start", "--bind", f"127.0.0.1:{port}", *extra]


def assert_gone(pid):
    def gone():
        return subprocess.run(["ps", "-p", str(pid), "-o", "pid="],
                              capture_output=True).returncode != 0
    eventually(gone)


with tempfile.TemporaryDirectory(prefix="xpost-agent-tests-") as temp:
    temp = Path(temp)
    a = workspace(temp / "worktree-a")
    b = workspace(temp / "worktree-b")
    device = temp / "device"
    device.touch()
    alias = temp / "device-alias"
    alias.symlink_to(device)
    env = {**os.environ, "XPOST_AGENT_TEST_DEVICE": str(device)}
    for key in ("XPOST_ROOT", "XPOST_PROFILES", "XPOST_AGENT_INTERNAL_START_ID"):
        env.pop(key, None)
    env_a = {
        **env, "XPOST_AGENT_TEST_RECONNECT_MS": "3000", "XPOST_AGENT_TEST_REQUEST_TIMEOUT_MS": "250",
    }
    env_b = {**env, "XPOST_AGENT_TEST_DEVICE": str(alias)}
    port = free_port()
    args = start_args(port)
    args_b = start_args(free_port())
    try:
        assert status(a, env_a)["state"] == "stopped"
        fails(a, ["health"], "Run xpost-agent start", env=env_a)
        start = subprocess.run(
            ["/bin/sh", "-c", f"{shlex.join([str(binary), *args])}; result=$?; exit $result"],
            cwd=a, env=env_a, text=True, capture_output=True, timeout=30,
        )
        assert start.returncode == 0, start.stderr
        first = json.loads(start.stdout)
        assert first["state"] == "running" and first["dut"] == "unknown", first
        instance = first["instance"]
        assert instance["config"]["worktree"] == str(a.resolve())
        assert instance["config"]["profiles"] == str((a / "profiles.toml").resolve())
        with socket.create_connection(("127.0.0.1", port), timeout=2) as abandoned:
            abandoned.sendall(b"GET /health HTTP/1.1\r\nHost: localhost\r\n")
        compare_health(instance)
        assert status(a, env_a) == first
        assert status(a, {**env_a, "XPOST_ROOT": "./missing"}) == first
        assert json.loads(run(a, args, env=env_a).stdout) == first
        fails(a, args + ["--log-dir", "different"], "running daemon differs", env=env_a)
        assert "HTTP daemon" in run(a, ["logs"], env=env_a).stdout
        assert http(instance["endpoint"], "/v1/status") == {"ok": True, "message": "idle"}
        assert http(instance["endpoint"], "/v1/get/map/rxcnt") == {
            "ok": True, "name": "rxcnt", "type": "array", "size": 2,
            "entries": [{"key": "0", "value": "42"}],
        }
        assert http(instance["endpoint"], "/v1/get/perf") == {
            "ok": True, "message": "OK get perf 4 b64=Q0FHVA==",
        }
        for path, body in [
            ("/mcp", {"jsonrpc": "2.0", "method": "initialize", "id": 1}),
            ("/control/stop", {"id": instance["id"]}),
        ]:
            try:
                http(instance["endpoint"], path, body)
            except HTTPError as error:
                assert error.code == 404, error
            else:
                raise AssertionError(f"unexpected route {path}")

        run(b, ["stop"], env=env_b)
        fails(b, ["enable", "xdp"], "no daemon for this Worktree", env=env_b)
        fails(b, ["restart", *args_b[1:]], "DUT is already held", env=env_b)
        fails(b, args_b, "DUT is already held", env=env_b)
        compare_health(instance)

        with ThreadPoolExecutor() as jobs:
            operation = jobs.submit(run, a, ["run", "fixture reconnect"], env=env_a, check=False)
            eventually(lambda: status(a, env_a)["dut"] == "busy")
            response = operation.result(timeout=4)
            assert response.returncode != 0 and "HTTP 408" in response.stderr, response
            assert status(a, env_a)["dut"] == "busy"
            fails(a, ["stop"], "device operations are still running", env=env_a)
            fails(a, ["restart", *args[1:]], "device operations are still running", env=env_a)
            fails(b, args_b, "DUT is already held", env=env_b)
            assert status(a, env_a)["instance"] == instance
            eventually(lambda: status(a, env_a)["dut"] == "unknown")
            assert "fixture DUT reconnected" in run(a, ["logs"], env=env_a).stdout

        run(a, ["stop"], env=env_a)
        assert status(a, env_a)["state"] == "stopped"
        expect_closed(instance["endpoint"])
        assert_gone(instance["pid"])
        print(f"shell exit, abandoned HTTP, and repeated start preserved {instance['id']} PID {instance['pid']}")
        print("HTTP 408 left the simulated reconnect busy and the DUT lock held")
        print("stop and restart refused while busy. Stop waited for process exit and lock release")

        with ThreadPoolExecutor(max_workers=4) as jobs:
            arrivals = list(jobs.map(lambda _: run(b, args_b, env=env_b), range(4)))
        contest = [json.loads(result.stdout)["instance"] for result in arrivals]
        assert all(contender == contest[0] for contender in contest)
        assert contest[0]["id"] != instance["id"]
        compare_health(contest[0])
        os.kill(contest[0]["pid"], signal.SIGKILL)
        assert_gone(contest[0]["pid"])
        assert status(b, env_b)["state"] == "stopped"
        recovered = json.loads(run(a, args, env=env_a).stdout)["instance"]
        assert recovered["id"] != instance["id"]
        compare_health(recovered)
        run(a, ["stop"], env=env_a)
        print(f"four concurrent starts produced {contest[0]['id']} PID {contest[0]['pid']}")
        print(f"alias access failed during contention and succeeded after stop and crash, recovery PID {recovered['pid']}")

        with socket.socket() as occupied:
            occupied.bind(("127.0.0.1", 0))
            occupied.listen()
            busy_port = occupied.getsockname()[1]
            with subprocess.Popen(["sleep", "25"]) as unrelated:
                try:
                    # Leave a record with a live but unrelated PID. Control never uses it.
                    digest = hashlib.sha256(os.fsencode(b.resolve())).hexdigest()[:32]
                    record = Path(f"/tmp/xpost-agent-{os.geteuid()}") / digest / "last.json"
                    record.write_text(json.dumps({**contest[0], "pid": unrelated.pid}))
                    run(b, ["stop"], env=env_b)
                    fails(b, start_args(busy_port), "Address already in use", env=env_b)
                    assert unrelated.poll() is None
                    with socket.create_connection(("127.0.0.1", busy_port), timeout=2):
                        pass
                finally:
                    unrelated.terminate()
        fails(a, args, "fixture DUT unavailable", env={**env_a, "XPOST_AGENT_TEST_OPEN_FAIL": "1"})
        fails(a, args, "readiness timed out", env={
            **env_a, "XPOST_AGENT_TEST_OPEN_DELAY_MS": "5000", "XPOST_AGENT_TEST_START_TIMEOUT_MS": "200",
        })
        assert status(a, env_a)["state"] == "stopped"
        expect_closed(instance["endpoint"])
        print("no-service stop, stale PID, port contention, failed initialization, and readiness timeout preserved isolation")

        caller = a / "caller"
        caller.mkdir()
        profile_override = a / "alternate.toml"
        shutil.copy2(a / "profiles.toml", profile_override)
        caller_env = {**env_a, "XPOST_ROOT": "..", "XPOST_PROFILES": "../alternate.toml"}
        with subprocess.Popen(
            [str(binary), "serve", "--bind", "127.0.0.1:0", "--log-dir", "../foreground-logs"],
            cwd=caller, env=caller_env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        ) as foreground:
            try:
                def foreground_ready():
                    result = run(a, ["status"], env=env_a, check=False)
                    return result.returncode == 0 and json.loads(result.stdout)["state"] == "running"

                eventually(foreground_ready)
                live = status(a, env_a)["instance"]
                assert live["pid"] == foreground.pid and not live["managed"]
                compare_health(live)
                assert live["config"]["root"] == str(a.resolve())
                assert live["config"]["profiles"] == str(profile_override.resolve())
                assert live["config"]["log_dir"] == str((a / "foreground-logs").resolve())
                fails(a, ["stop"], "stop the foreground daemon", env=env_a)
            finally:
                foreground.send_signal(signal.SIGINT)
                assert foreground.wait(timeout=5) == 0
        expect_closed(live["endpoint"])
        print("foreground serve resolved paths from the caller and exited on SIGINT")

        if alternate:
            live_path = temp / "replaceable-agent"
            selection = temp / "next-agent"
            shutil.copy2(binary, live_path)
            shutil.copy2(alternate, selection)
            build_a = json.loads(run(a, args, env=env_a, executable=live_path).stdout)["instance"]
            broken = subprocess.run(
                ["cargo", "build", "--offline", "--locked", "--target-dir", str(temp / "failed-build")],
                cwd=repository / "xpost-agent", capture_output=True, text=True, timeout=10,
                env={**os.environ, "RUSTFLAGS": "-C xpost-intentional-build-failure"},
            )
            assert broken.returncode != 0 and "unknown codegen option" in broken.stderr, broken
            assert status(a, env_a, executable=live_path)["instance"] == build_a
            selection.replace(live_path)
            assert status(a, env_a, executable=live_path)["instance"] == build_a
            fails(a, args, "running daemon differs", executable=live_path, env=env_a)
            fails(a, ["enable", "xdp"], "Run xpost-agent restart", executable=live_path, env=env_a)
            assert "HTTP daemon" in run(a, ["logs"], executable=live_path, env=env_a).stdout
            build_b = json.loads(run(a, ["restart", *args[1:]], executable=live_path, env=env_a).stdout)["instance"]
            assert build_a["build"]["git_revision"] == build_b["build"]["git_revision"]
            assert build_a["build"]["image_id"] != build_b["build"]["image_id"]
            assert build_a["id"] != build_b["id"]
            assert_gone(build_a["pid"])
            print(f"same-revision replacement identified {build_a['build']['image_id']} PID {build_a['pid']} "
                  f"and {build_b['build']['image_id']} PID {build_b['pid']}")
            run(a, ["stop"], executable=live_path, env=env_a)
    finally:
        run(a, ["stop"], env=env_a, check=False)
        run(b, ["stop"], env=env_b, check=False)
