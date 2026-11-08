#!/usr/bin/env python3
"""Run the exec_bench workloads against the interpreter engine on the board.

For each workload this packs the eBPF bytecode into an XDP ELF container, writes
it to the cold table, resets, runs one `enable exec`, and records the segment
timings and the output bytes. Results land as JSON on stdout.

The container carries the `bpf.o` xdp section, not the AOT machine code: the
interpreter cannot execute RV32 instructions, and the same metadata still passes
the name, map and digest checks.
"""

import json
import re
import subprocess
import sys
import time
from pathlib import Path

import serial

PORT = "/dev/cu.usbmodem21101"
TABLE_OFFSET = "0x310000"
BUILD = Path("/Volumes/SSD/bs/build")
PACK = Path(
    "/Volumes/SSD/bs/libbpf-mcu/tools/target/aarch64-apple-darwin/release/xpost-mcu-elf"
)
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

ANSI = re.compile(r"\x1b\[[0-9;]*m")
SEG = re.compile(
    r"exec seg: load=(\d+) lease=(\d+) engine=(\d+) release=(\d+) store=(\d+) total=(\d+)"
)


def drain(ser, seconds):
    buf = b""
    end = time.time() + seconds
    while time.time() < end:
        data = ser.read(8192)
        if data:
            buf += data
    return ANSI.sub("", buf.decode("utf-8", "replace"))


def pack(workload):
    """Pack one workload's eBPF bytecode into a container, return its path."""
    obj = BUILD / workload / "bpf.o"
    if not obj.exists():
        raise SystemExit(f"missing {obj}; build libbpf-mcu for {workload} first")
    section = Path(f"/tmp/{workload}_ebpf.bin")
    subprocess.run(
        ["llvm-objcopy", "-O", "binary", "--only-section=xdp", str(obj), str(section)],
        check=True,
    )
    out = Path(f"/tmp/{workload}_interp.elf")
    subprocess.run(
        [
            str(PACK),
            "pack",
            "--image", str(section),
            "--bpf-toml", str(BUILD / workload / "bpf.toml"),
            "--name", workload,
            "--slot-address", "0x40380200",
            "--helper-address", "0x40380000",
            "--machine", "1",
            "-o", str(out),
        ],
        check=True,
    )
    return out


def flash_container(path):
    subprocess.run(
        ["espflash", "write-bin", "--port", PORT, TABLE_OFFSET, str(path)],
        check=True,
        capture_output=True,
    )


def reset(ser):
    ser.dtr = False
    ser.rts = True
    time.sleep(0.2)
    ser.rts = False
    time.sleep(0.8)


def main():
    results = []
    ser = serial.Serial(PORT, 115200, timeout=0.3)
    try:
        for name, input_hex in WORKLOADS:
            container = pack(name)
            flash_container(container)
            reset(ser)
            boot = drain(ser, 3.5)
            switched = "xdp switched, not leased" in boot

            ser.write((f"update map workload 0 {input_hex}\n").encode())
            time.sleep(0.4)
            drain(ser, 1.5)
            ser.write(b"clear perf\n")
            time.sleep(0.3)
            drain(ser, 1.0)
            ser.write(b"enable exec\n")
            time.sleep(0.4)
            body = drain(ser, 8.0)

            seg = SEG.search(body)
            blob = re.search(r"OK get map (\d+)\n((?:[0-9a-f ]+\n?)+)", body)
            results.append(
                {
                    "workload": name,
                    "switched": switched,
                    "ok": "OK enable exec" in body,
                    "seg": None
                    if not seg
                    else {
                        k: int(seg.group(i + 1))
                        for i, k in enumerate(
                            ["load", "lease", "engine", "release", "store", "total"]
                        )
                    },
                }
            )
            print(f"{name:11s} switched={switched} ok={results[-1]['ok']} seg={results[-1]['seg']}",
                  file=sys.stderr)
    finally:
        ser.close()

    json.dump(results, sys.stdout, indent=2)


if __name__ == "__main__":
    main()
