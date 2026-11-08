#!/usr/bin/env python3
"""Measure all eight workloads on one engine.

Each workload is its own image, so the host swaps the image, writes the matching
input, triggers `enable exec`, and reads the single `bench` line the firmware
prints. A fresh serial handle per workload keeps one workload's leftover output
from being read as the next one's result.

Usage: `workload_bench.py <aot|interp>` writes JSON on stdout.

The input values mirror `xpost_perf::sweep::FIXTURES`. A wrong input makes the
program return `XDP_ABORTED`, which shows up in the `act` field rather than as a
silent wrong number.
"""

import json
import re
import subprocess
import sys
import time
from pathlib import Path

import serial

PORT = "/dev/cu.usbmodem21101"
TABLE = "0x310000"
BUILD = Path("/Volumes/SSD/bs/build")
PACK = Path(
    "/Volumes/SSD/bs/libbpf-mcu/tools/target/aarch64-apple-darwin/release/xpost-mcu-elf"
)

# name: (opcode, param, payload). Mirrors `xpost_perf::sweep::FIXTURES`.
FIXTURES = {
    "incr": (0, 0, []),
    "square": (1, 3, [3, 0, 0, 0]),
    "bitswap": (2, 0, [0x5A, 0xA5, 0xA5, 0x5A]),
    "fib": (3, 30, []),
    "buf": (4, 16, [3, 0x14, 0x25, 0x36, 0x47, 0x58, 0x69, 0x7A]),
    "memcpy": (5, 8, [3, 0x14, 0x25, 0x36, 0x47, 0x58, 0x69, 0x7A]),
    "fletcher32": (6, 16, [3, 0x14, 0x25, 0x36, 0x47, 0x58, 0x69, 0x7A]),
    "bsort": (7, 6, [0x9D, 0x60, 0x15, 0x17, 0xE4, 0xE6, 0xDD, 0x78]),
}

ANSI = re.compile(rb"\x1b\[[0-9;]*m")
BENCH = re.compile(r"bench runs=(\d+) mean_ns=(\d+) act=(\S+)")


def value_hex(name):
    opcode, param, payload = FIXTURES[name]
    v = bytearray(32)
    v[0:4] = opcode.to_bytes(4, "little")
    v[4:8] = param.to_bytes(4, "little")
    v[8:8 + len(payload)] = bytes(payload)
    return v.hex()


def container(name, engine):
    if engine == "aot":
        image = BUILD / name / "esp32c3.bin"
    else:
        section = Path(f"/tmp/{name}_b.bin")
        subprocess.run(
            ["llvm-objcopy", "-O", "binary", "--only-section=xdp",
             str(BUILD / name / "bpf.o"), str(section)],
            check=True,
        )
        image = section
    out = Path(f"/tmp/{name}_{engine}.elf")
    res = subprocess.run(
        [str(PACK), "pack", "--image", str(image),
         "--bpf-toml", str(BUILD / name / "bpf.toml"),
         "--name", name, "--slot-address", "0x40380200",
         "--helper-address", "0x40380000", "--machine", "1", "-o", str(out)],
        capture_output=True,
    )
    if res.returncode != 0:
        raise RuntimeError(res.stderr.decode("utf-8", "replace").strip())
    return out


def measure(name, engine):
    """Install the image, seed the input, run the bench, return the mean."""
    blob = container(name, engine)
    for _ in range(3):
        res = subprocess.run(
            ["espflash", "write-bin", "--port", PORT, TABLE, str(blob)],
            capture_output=True,
        )
        if res.returncode == 0:
            break
        time.sleep(2)
    else:
        raise RuntimeError(f"flash failed: {res.stderr.decode('utf-8', 'replace')[:200]}")

    ser = serial.Serial(PORT, 115200, timeout=0.3)
    try:
        ser.dtr = False
        ser.rts = True
        time.sleep(0.2)
        ser.rts = False
        time.sleep(1.2)

        # Drain until the boot banner, which the App prints after the scale sweep
        # and after it has published the image.
        seen = b""
        deadline = time.time() + 45
        while time.time() < deadline:
            data = ser.read(8192)
            if data:
                seen += data
            if b"OK init" in ANSI.sub(b"", seen):
                break
        rest = ser.read(8192)
        if rest:
            seen += rest

        ser.write(f"update map workload 0 {value_hex(name)}\n".encode())
        time.sleep(0.5)
        ack = ser.read(4096)
        if b"OK update map" not in ack:
            return None, "update-failed"

        ser.write(b"enable exec\n")
        time.sleep(0.3)
        buf = b""
        deadline = time.time() + 40
        while time.time() < deadline:
            data = ser.read(8192)
            if data:
                buf += data
            m = BENCH.search(ANSI.sub(b"", buf).decode("utf-8", "replace"))
            if m:
                runs, mean, act = int(m.group(1)), int(m.group(2)), m.group(3)
                return {"runs": runs, "mean_ns": mean, "act": act}, None
            if b"OK enable exec" in buf:
                time.sleep(0.5)
        return None, "no-bench-line"
    finally:
        ser.close()


def main():
    engine = sys.argv[1] if len(sys.argv) > 1 else "interp"
    rows = []
    for name in FIXTURES:
        try:
            got, err = measure(name, engine)
        except RuntimeError as e:
            got, err = None, str(e)
        if got is None:
            rows.append({"workload": name, "error": err})
            print(f"{name:11s} error={err}", file=sys.stderr)
        else:
            rows.append({"workload": name, **got})
            print(f"{name:11s} mean_ns={got['mean_ns']} act={got['act']}", file=sys.stderr)
    json.dump({"engine": engine, "rows": rows}, sys.stdout, indent=2)


if __name__ == "__main__":
    main()
