#!/usr/bin/env python3
"""Measure the interpreter engine alone, per workload, on the board.

Uses the `xpost-perf` report: `get perf` returns a self-describing blob whose
`engine` entry is the engine call between the `IN_FLIGHT` claim and its release.
That excludes the counter and trace hooks on both sides, which measured about
15 microseconds per flight against a program that takes about 100.

Writes JSON on stdout, one record per workload.
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
REPEATS = 5
WORKLOADS = [
    ("incr", "0000000000000000000000000000000000000000000000000000000000000000"),
    ("square", "0100000000000000030000000000000000000000000000000000000000000000"),
    ("bitswap", "02000000000000005a5aa5a50000000000000000000000000000000000000000"),
    ("fib", "030000001e000000000000000000000000000000000000000000000000000000"),
    ("buf", "0400000010000000031425364758697a8b9cadbecfe0f1020000000000000000"),
    ("memcpy", "0500000008000000031425364758697a00000000000000000000000000000000"),
    ("fletcher32", "0600000010000000031425364758697a8b9cadbecfe0f1020000000000000000"),
    ("bsort", "07000000060000009d601517e4e6dd782b6da6da72f36e3cb979379e00000000"),
]

ANSI = re.compile(rb"\x1b\[[0-9;]*m")
HEAD = re.compile(rb"OK get perf (\d+)\n")


def pack(name):
    obj = BUILD / name / "bpf.o"
    if not obj.exists():
        raise RuntimeError(f"missing {obj}")
    section = Path(f"/tmp/{name}_ebpf.bin")
    subprocess.run(
        ["llvm-objcopy", "-O", "binary", "--only-section=xdp", str(obj), str(section)],
        check=True,
    )
    out = Path(f"/tmp/{name}_interp.elf")
    # A program larger than the container cap cannot be packed. That is a
    # format limit, not a measurement failure, so report it as a skip.
    res = subprocess.run(
        [
            str(PACK), "pack",
            "--image", str(section),
            "--bpf-toml", str(BUILD / name / "bpf.toml"),
            "--name", name,
            "--slot-address", "0x40380200",
            "--helper-address", "0x40380000",
            "--machine", "1",
            "-o", str(out),
        ],
        capture_output=True,
    )
    if res.returncode != 0:
        err = res.stderr.decode("utf-8", "replace").strip()
        raise RuntimeError(f"{err} ({section.stat().st_size} byte program)")
    return out


def read_report(ser):
    """`get perf` and decode the `xpost-perf` blob."""
    ser.write(b"get perf\n")
    time.sleep(0.4)
    buf = b""
    end = time.time() + 4
    while time.time() < end:
        data = ser.read(8192)
        if data:
            buf += data
        m = HEAD.search(ANSI.sub(b"", buf))
        if m:
            start = m.end()
            want = int(m.group(1))
            if len(buf) - start >= want:
                blob = ANSI.sub(b"", buf)[start:start + want]
                return decode(blob)
    return None


def decode(blob):
    if blob[:4] != b"PFR1":
        raise SystemExit(f"unexpected magic {blob[:4]!r}")
    count = int.from_bytes(blob[8:10], "little")
    o = 10
    out = {}
    for _ in range(count):
        n = blob[o]
        o += 1
        name = blob[o:o + n].decode()
        o += n
        kind = blob[o]
        o += 1
        if kind == 2:  # span
            cnt = int.from_bytes(blob[o:o + 8], "little")
            tot = int.from_bytes(blob[o + 8:o + 16], "little")
            mn = int.from_bytes(blob[o + 16:o + 24], "little")
            mx = int.from_bytes(blob[o + 24:o + 32], "little")
            o += 32
            out[name] = {"count": cnt, "mean": tot // cnt if cnt else None, "min": mn if cnt else None, "max": mx}
        elif kind == 1:  # counter
            out[name] = int.from_bytes(blob[o:o + 8], "little")
            o += 8
        else:
            raise SystemExit(f"unexpected kind {kind}")
    return out


def main():
    results = []
    ser = serial.Serial(PORT, 115200, timeout=0.3)
    try:
        for name, input_hex in WORKLOADS:
            try:
                container = pack(name)
            except RuntimeError as e:
                print(f"{name}: skip ({e})", file=sys.stderr)
                results.append({"workload": name, "skipped": str(e)})
                continue
            subprocess.run(
                ["espflash", "write-bin", "--port", PORT, TABLE, str(container)],
                check=True, capture_output=True,
            )
            ser.dtr = False
            ser.rts = True
            time.sleep(0.2)
            ser.rts = False
            time.sleep(0.9)
            ser.read(65536)

            samples = []
            for _ in range(REPEATS):
                ser.write((f"update map workload 0 {input_hex}\n").encode())
                time.sleep(0.3)
                ser.read(4096)
                ser.write(b"clear perf\n")
                time.sleep(0.2)
                ser.read(4096)
                ser.write(b"enable exec\n")
                time.sleep(0.4)
                ser.read(8192)
                rep = read_report(ser)
                if rep and "engine" in rep and rep["engine"]["mean"] is not None:
                    samples.append(rep["engine"]["mean"])
            results.append(
                {
                    "workload": name,
                    "engine_us": samples,
                    "engine_min": min(samples) if samples else None,
                }
            )
            print(f"{name:11s} n={len(samples)} engine={samples}", file=sys.stderr)
    finally:
        ser.close()
    json.dump(results, sys.stdout, indent=2)


if __name__ == "__main__":
    main()
