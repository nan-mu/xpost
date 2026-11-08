#!/usr/bin/env python3
"""Sweep one workload over its work size, on one engine.

The installed image decides the workload and the frame length decides the work,
so this installs the image once, then for every size in the sweep writes a frame
of that length and reads the mean the firmware prints.

# Which workloads can sweep

Only two take a free work size:

- `buf` copies `param` bytes, so `param` is the work.
- `fib` loops `param` times, clamped at 47.

The others clamp hard (`memcpy` 12, `fletcher32` 20, `bsort` 6) or ignore the
parameter (`incr`, `square`, `bitswap`), so a sweep over them would measure the
same program repeatedly. This script refuses those rather than plotting a flat
line and calling it a result.

Usage: `scale_sweep.py <workload> <aot|interp>` writes JSON on stdout.
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

# Workload opcode, and the largest work size its source accepts.
OPCODE = {
    "incr": 0, "square": 1, "bitswap": 2, "fib": 3,
    "buf": 4, "memcpy": 5, "fletcher32": 6, "bsort": 7,
}
SWEEPABLE = {"buf", "fib"}

# Powers of two, plus 47 for `fib`, whose clamp lands between 32 and 64.
# Mirrors `xpost_perf::sweep::PARAMS`; the firmware sweeps this union and each
# workload reports the sizes its source accepts.
PARAMS_BUF = [1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 47, 64, 128, 256, 512, 1024]
PARAMS_FIB = [1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 47]

RUNS = 200
ANSI = re.compile(rb"\x1b\[[0-9;]*m")
SWEEP = re.compile(r"sweep param=(\d+) runs=\d+ mean_ns=(\d+) act=(\S+)")


def container(workload, engine):
    if engine == "aot":
        image = BUILD / workload / "esp32c3.bin"
    else:
        section = Path(f"/tmp/{workload}_sw.bin")
        subprocess.run(
            ["llvm-objcopy", "-O", "binary", "--only-section=xdp",
             str(BUILD / workload / "bpf.o"), str(section)],
            check=True,
        )
        image = section
    out = Path(f"/tmp/{workload}_{engine}_sw.elf")
    res = subprocess.run(
        [str(PACK), "pack", "--image", str(image),
         "--bpf-toml", str(BUILD / workload / "bpf.toml"),
         "--name", workload, "--slot-address", "0x40380200",
         "--helper-address", "0x40380000", "--machine", "1", "-o", str(out)],
        capture_output=True,
    )
    if res.returncode != 0:
        raise RuntimeError(res.stderr.decode("utf-8", "replace").strip())
    return out


def main():
    workload = sys.argv[1]
    engine = sys.argv[2]
    if workload not in SWEEPABLE:
        raise SystemExit(
            f"{workload} does not take a free work size; sweepable: {sorted(SWEEPABLE)}"
        )

    blob = container(workload, engine)
    res = subprocess.run(
        ["espflash", "write-bin", "--port", PORT, TABLE, str(blob)],
        capture_output=True,
    )
    if res.returncode != 0:
        raise SystemExit(f"flash failed: {res.stderr.decode('utf-8', 'replace')[:200]}")

    expected = set(PARAMS_BUF if workload == "buf" else PARAMS_FIB)
    ser = serial.Serial(PORT, 115200, timeout=0.3)
    rows = []
    try:
        ser.dtr = False
        ser.rts = True
        time.sleep(0.2)
        ser.rts = False
        time.sleep(1.2)

        # The firmware sweeps on every boot. Read until every expected size has
        # reported or the budget runs out.
        buf = b""
        deadline = time.time() + 240
        seen = set()
        while time.time() < deadline and seen != expected:
            data = ser.read(8192)
            if not data:
                continue
            buf += data
            text = ANSI.sub(b"", buf).decode("utf-8", "replace")
            for m in SWEEP.finditer(text):
                param = int(m.group(1))
                if param in seen:
                    continue
                if param not in expected:
                    continue
                seen.add(param)
                rows.append({"param": param, "mean_ns": int(m.group(2)), "act": m.group(3)})
        missing = expected - seen
    finally:
        ser.close()

    rows.sort(key=lambda r: r["param"])
    json.dump(
        {"workload": workload, "engine": engine, "runs": RUNS,
         "rows": rows, "missing": sorted(missing)},
        sys.stdout, indent=2,
    )


if __name__ == "__main__":
    main()
