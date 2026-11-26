"""Exercise the foreground binary and real routes with a test-device build."""

import base64
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


binary = str(Path(sys.argv[1]).resolve())
FIRMWARE = b"host fixture Firmware ELF bytes"
DIGEST = hashlib.sha256(FIRMWARE).hexdigest()
EXB2 = b"EXB2\x02\x00\x40\x00" + bytes((i % 251 for i in range(8250)))


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def serve(port):
    return [binary, "serve", "--bind", f"127.0.0.1:{port}"]


def call(base, route, body=None, headers=None):
    data = json.dumps(body).encode() if body is not None else None
    request = Request(base + route, data, headers={
        "Content-Type": "application/json", **(headers or {}),
    })
    with urlopen(request, timeout=4) as reply:
        return json.load(reply)


def cli(base, *args, success=True):
    result = subprocess.run(
        [binary, "--endpoint", base, *args], capture_output=True, text=True, timeout=10,
    )
    assert (result.returncode == 0) == success, (args, result.stdout, result.stderr)
    return json.loads(result.stdout) if result.stdout else result.stderr


def until(check, seconds=5):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            if check():
                return
        except URLError:
            pass
        time.sleep(0.02)
    raise AssertionError("condition did not become true")


@contextmanager
def daemon(root, env):
    port = free_port()
    base = f"http://127.0.0.1:{port}"
    with subprocess.Popen(serve(port), cwd=root, env=env, stdout=subprocess.DEVNULL,
                          stderr=subprocess.PIPE, text=True) as proc:
        try:
            until(lambda: call(base, "/health") == {"ok": True, "message": "ok"})
            yield proc, base
        finally:
            if proc.poll() is None:
                proc.send_signal(signal.SIGINT)
            assert proc.wait(timeout=10) == 0, proc.stderr.read()


def worktree(path):
    path.mkdir()
    (path / "profiles.toml").write_text("""
[[fw]]
name = "fixture-hal"
target = "esp32c3"
partition_table = "partitions.csv"
artifact = "fixture.elf"
build = { cwd = ".", command = ["/bin/sh", "-c", 'echo build >> "$XPOST_AGENT_TEST_EVENTS"; sleep 0.4'] }
""")
    (path / "partitions.csv").write_text(
        "nvs,data,nvs,0x9000,0x6000,\nfactory,app,factory,0x10000,0x300000,\n"
        "xdp,0x40,0,0x310000,0x10000,\n"
    )
    (path / "fixture.elf").write_bytes(FIRMWARE)
    return path


def timeout(f, *args):
    try:
        f(*args)
    except HTTPError as error:
        assert error.code == 408, error
    else:
        raise AssertionError("expected HTTP 408")


with tempfile.TemporaryDirectory(prefix="xpost-agent-serve-") as temp:
    temp = Path(temp)
    a, b = worktree(temp / "a"), worktree(temp / "b")
    device = temp / "device"
    device.touch()
    alias = temp / "alias"
    alias.symlink_to(device)
    log = temp / "dut-events"
    helpers = temp / "bin"
    helpers.mkdir()
    gh = helpers / "gh"
    gh.write_text("#!/bin/sh\nexit 2\n")
    gh.chmod(0o700)
    env = {**os.environ, "XPOST_AGENT_TEST_DEVICE": str(device),
           "XPOST_AGENT_TEST_EVENTS": str(log), "PATH": f"{helpers}:{os.environ['PATH']}"}
    for key in ("XPOST_ROOT", "XPOST_PROFILES"):
        env.pop(key, None)

    def events():
        return log.read_text().splitlines()

    with daemon(a, env) as (proc, base):
        for _ in range(3):
            assert cli(base, "health") == {"ok": True, "message": "ok"}
        assert events() == ["board-info"]
        assert cli(base, "status")["message"] == "idle"
        contender = subprocess.run(serve(free_port()), cwd=b,
            env={**env, "XPOST_AGENT_TEST_DEVICE": str(alias)},
            text=True, capture_output=True, timeout=5)
        assert contender.returncode != 0 and "exclusive DUT lock" in contender.stderr
        assert events() == ["board-info"]
        try:
            call(base, "/v1/enable/xdp", {}, {"X-Xpost-Worktree": "other"})
        except HTTPError as error:
            assert error.code == 409
        else:
            raise AssertionError("wrong Worktree accepted")
        for invalid in [{}, {"name": ""}, {"name": "  "}, {"name": []},
                        {"name": ["", " "]}, {"name": ["valid", " "]},
                        {"name": ["valid", "*"]}, {"name": ["valid", "missing"]}]:
            previous = events()
            assert not call(base, "/v1/clear/xdp", invalid)["ok"], invalid
            assert events() == previous, invalid
        for names in [("", " "), ("valid", "missing"), ("valid", "*")]:
            assert not cli(base, "clear", "xdp", *names, success=False)["ok"]
        assert not any(event.startswith("erase") for event in events())
        for names in [("valid",), ("valid", "other"), ("*",)]:
            assert cli(base, "clear", "xdp", *names)["ok"]
        assert call(base, "/v1/clear/xdp", {"name": ["*"]})["ok"]
        assert events()[1:] == ['erase ["valid"]', 'erase ["valid", "other"]', 'erase []', 'erase []']

        bad = cli(base, "flash", "fw", "fixture-hal", "--expected-sha256", "0" * 64, success=False)
        assert "changed after preflight" in bad["message"]
        assert "flash firmware" not in events()
        with ThreadPoolExecutor() as jobs:
            pending = jobs.submit(cli, base, "flash", "fw", "fixture-hal", "--expected-sha256", DIGEST)
            until(lambda: events().count("build") == 2)
            assert cli(base, "status")["message"] == "busy"
            assert cli(base, "clear", "xdp", "valid")["ok"]
            assert pending.result()["flash"] == {
                "firmware_profile": "fixture-hal", "firmware_sha256": DIGEST, "containers": [],
            }
        assert events()[-3:] == ["build", "flash firmware", 'erase ["valid"]']
        for bad in [
            {"name": "a", "firmware": "fixture-hal"},
            {"name": "a", "slot": 1, "firmware": "fixture-hal", "expected_sha256": DIGEST},
        ]:
            assert not call(base, "/v1/flash/xdp", bad)["ok"]
        assert cli(base, "flash", "xdp", "case_mark", "--slot", "1")["message"].startswith("OK xdp flash 1 9 0")
        for generation, prefix in [(9, "1 0 0"), (8, "0 1 0"), (7, "0 0 1"), (6, "0 0 0")]:
            result = cli(base, "xdp", "switch", "--slot", "1", "--generation", str(generation),
                         "--policy", "pass", success=generation == 9)
            assert result["message"].startswith(f"OK xdp switch {prefix} 0 1 {generation} 0 1 123 agent_rx=")
        assert "outcome unknown" in cli(
            base, "xdp", "switch", "--slot", "1", "--generation", "99",
            "--policy", "drop", success=False)["message"]
        assert events().count("switch") == 5
        assert cli(base, "xdp", "status")["message"].startswith("OK get xdp 42 0 1 ")

        for variant in ("metrics", "trace"):
            prepared = cli(base, "perf", "prepare", "--uuid", "0" * 32, "--profile", DIGEST, "--variant", variant)
            assert prepared["message"] == "OK perf prepare epoch=7 boot_id=42"
            assert cli(base, "perf", "arm", "--epoch", "7")["message"] == "OK perf arm tick=21"
            freeze = cli(base, "perf", "freeze", "--epoch", "7")["message"]
            digest = 0x811c9dc5
            for byte in b"9" * 600:
                digest = ((digest ^ byte) * 0x01000193) & 0xffffffff
            assert freeze == f"OK perf freeze tick=24 flags=2 len=600 digest={digest}"
            chunks = []
            for offset, size in [(0, 512), (512, 88)]:
                reply = cli(base, "perf", "read", "--epoch", "7", "--offset", str(offset),
                            "--max-bytes", "512")["message"]
                assert reply.startswith(f"OK perf read total=600 digest={digest} n={size} b64=")
                chunks.append(base64.b64decode(reply.split(" b64=")[1]))
            assert b"".join(chunks) == b"9" * 600
            assert cli(base, "perf", "release", "--epoch", "7")["message"] == "OK perf release"
        assert not cli(base, "perf", "prepare", "--uuid", "00", "--profile", DIGEST,
                       "--variant", "trace", success=False)["ok"]

        image = temp / "exb2.bin"
        image.write_bytes(EXB2)
        assert not cli(base, "exec", "batch", "--input-hex", "00", success=False)["ok"]
        assert "exec reset" not in events()
        batch = cli(base, "exec", "batch", "--input-file", str(image))["result"]
        assert base64.b64decode(batch.pop("output")) == b"Z" * 8258
        assert batch == {"boot_id": 42, "begin_tick": 20, "end_tick": 30, "execution_return": 2}
        assert events().count("exec invoke") == 1
        received = bytearray()
        for event in events():
            if event.startswith("exec write "):
                _, _, offset, data = event.split()
                assert int(offset) == len(received)
                received.extend(bytes.fromhex(data))
        assert received == EXB2 and len(received) == 8258
        assert cli(base, "get", "map", "rxcnt")["entries"] == [{"key": "0", "value": "42"}]
        report = cli(base, "preflight", "--remote", success=False)["preflight"]
        assert {"release", "required", "deployments", "issues", "prepared", "hal_firmware"} <= report.keys()
        assert not cli(base, "preflight", "--remote", "--xdp", "missing", success=False)["ok"]
    print("Board Info success, CLI/HTTP contracts, clear safety, operation serialization, and SIGINT passed")

    log.unlink()
    failed_port = free_port()
    failed = subprocess.run(serve(failed_port), cwd=a, env={**env, "XPOST_AGENT_TEST_BOARD_FAIL": "1"},
                            capture_output=True, text=True, timeout=5)
    assert failed.returncode != 0 and "Board Info failed" in failed.stderr
    assert events() == ["board-info"]
    try:
        call(f"http://127.0.0.1:{failed_port}", "/health")
    except URLError:
        pass
    else:
        raise AssertionError("failed startup exposed health")

    slow = {**env, "XPOST_AGENT_TEST_DEVICE": str(alias), "XPOST_AGENT_TEST_REQUEST_TIMEOUT_MS": "250"}
    with daemon(b, slow) as (proc, base):
        with ThreadPoolExecutor() as jobs:
            arm = jobs.submit(timeout, call, base, "/v1/perf/arm", {"epoch": 99})
            until(lambda: "slow arm begin" in events())
            assert cli(base, "status")["message"] == "busy"
            arm.result()
            assert cli(base, "status")["message"] == "busy"
            until(lambda: cli(base, "status")["message"] == "idle")
            arm = jobs.submit(timeout, call, base, "/v1/perf/arm", {"epoch": 99})
            until(lambda: events().count("slow arm begin") == 2)
            queued = jobs.submit(timeout, call, base, "/v1/perf/freeze", {"epoch": 99})
            arm.result()
            queued.result()
            assert cli(base, "status")["message"] == "busy"
            proc.send_signal(signal.SIGTERM)
            assert proc.poll() is None
            assert proc.wait(timeout=5) == 0, proc.stderr.read()
            assert events()[-2:] == ["slow arm end", "freeze"]
    with daemon(a, {**env, "XPOST_AGENT_TEST_INVOKE_TIMEOUT": "1"}) as (_, base):
        unknown = cli(base, "exec", "batch", "--input-hex", EXB2.hex(), success=False)
        assert "EXB2 invocation outcome unknown" in unknown["message"]
        assert events().count("exec invoke") == 1
    print("Board Info failure, alias lock release, HTTP timeout and busy/idle, SIGTERM drain, and EXB2 unknown outcome passed")
