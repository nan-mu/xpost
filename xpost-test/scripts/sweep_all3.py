#!/usr/bin/env python3
"""Sweep every exec_bench workload over its work size, on all three engines.

The workload and its sizes come from the opcode in the seeded value, so one
firmware build sweeps its installed workload without per-workload firmware.

# Uniformity

Every (workload, engine) pair goes through the same loop: install the image, seed
the value, send `enable exec`, read the sweep lines, send `disable exec`. The
HAL arm installs nothing because its firmware carries all eight workloads, so it
only seeds and triggers.

# Why the fixed-work workloads are included

`incr`, `square` and `bitswap` do not use their parameter, so their sweep is
flat. That line is the measurement: it is the fixed cost of a call with almost no
work inside, which is exactly the intercept the other figures have to subtract.

Usage: `sweep_all3.py <aot|interp|hal>` writes one JSON per repeat under
`/tmp/sweeps3/`.
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
OUT = Path("/tmp/sweeps3")
REPEATS = 5

FIRMWARE = {
    "aot": "/tmp/exec_aot_diag",
    "interp": "/tmp/exec_interp_diag",
    "hal": "/Volumes/SSD/bs/xpost/examples/exec_bench_hal/target/riscv32imc-unknown-none-elf/release/exec_bench_hal",
}

# name: (opcode, sizes) mirrors `xpost_perf::sweep::params_for`.
WORKLOADS = {
    "incr": (0, [1, 2, 4, 8]),
    "square": (1, [1, 2, 4, 8]),
    "bitswap": (2, [1, 2, 4, 8]),
    "fib": (3, [1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 47]),
    "buf": (4, [1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 47, 64, 128, 256, 512, 1024]),
    "memcpy": (5, [1, 2, 4, 6, 9, 12]),
    "fletcher32": (6, [1, 2, 4, 8, 12, 16, 20]),
    "bsort": (7, [2, 3, 4, 5, 6]),
}

ANSI = re.compile(rb"\x1b\[[0-9;]*m")
SWEEP = re.compile(r"sweep (?:wl=\w+ )?op=\d+ param=(\d+) runs=\d+ mean_ns=(\d+) act=(\S+)|sweep wl=\w+ param=(\d+) runs=\d+ mean_ns=(\d+)")


def value_hex(opcode, param):
    v = bytearray(32)
    v[0:4] = opcode.to_bytes(4, "little")
    v[4:8] = param.to_bytes(4, "little")
    for i in range(min(param, 20)):
        v[8 + i] = (0x10 + i) & 0xFF
    return v.hex()


def container(workload, engine, opcode, param):
    """A container for one workload, sized for the sweep's largest frame."""
    if engine == "aot":
        image = BUILD / workload / "esp32c3.bin"
    else:
        section = Path(f"/tmp/{workload}_s3.bin")
        subprocess.run(
            ["llvm-objcopy", "-O", "binary", "--only-section=xdp",
             str(BUILD / workload / "bpf.o"), str(section)],
            check=True,
        )
        image = section
    out = Path(f"/tmp/{workload}_{engine}_s3.elf")
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


def flash_firmware(engine):
    res = subprocess.run(
        ["espflash", "flash", "--port", PORT, FIRMWARE[engine]],
        capture_output=True,
    )
    if res.returncode != 0:
        raise SystemExit(f"flash {engine}: {res.stderr.decode('utf-8', 'replace')[:200]}")


def run_workload(engine, name, opcode, sizes):
    """Install, seed, trigger, read the sweep. Returns {param: mean_ns}."""
    if engine != "hal":
        blob = container(name, engine, opcode, max(sizes))
        res = subprocess.run(
            ["espflash", "write-bin", "--port", PORT, TABLE, str(blob)],
            capture_output=True,
        )
        if res.returncode != 0:
            raise RuntimeError(f"flash xdp: {res.stderr.decode('utf-8', 'replace')[:160]}")

    ser = serial.Serial(PORT, 115200, timeout=0.3)
    try:
        ser.dtr = False
        ser.rts = True
        time.sleep(0.2)
        ser.rts = False
        time.sleep(1.2)

        seen = b""
        deadline = time.time() + 45
        while time.time() < deadline:
            data = ser.read(8192)
            if data:
                seen += data
            if b"OK init" in ANSI.sub(b"", seen):
                break
        ser.read(8192)

        # Seed with the first size; the firmware reads the opcode from here.
        ser.write(f"update map workload 0 {value_hex(opcode, sizes[0])}\n".encode())
        time.sleep(0.5)
        if b"OK update map" not in ser.read(4096):
            return {}

        ser.write(b"enable exec\n")
        time.sleep(0.3)
        buf = b""
        got = {}
        deadline = time.time() + 300
        while time.time() < deadline and len(got) < len(sizes):
            data = ser.read(8192)
            if not data:
                continue
            buf += data
            text = ANSI.sub(b"", buf).decode("utf-8", "replace")
            for m in SWEEP.finditer(text):
                if m.group(1) is not None:
                    p, ns = int(m.group(1)), int(m.group(2))
                else:
                    p, ns = int(m.group(4)), int(m.group(5))
                if p in sizes and p not in got:
                    got[p] = ns
        ser.write(b"disable exec\n")
        time.sleep(0.3)
        ser.read(4096)
        return got
    finally:
        ser.close()


def collect_hal_all():
    """HAL sweeps every workload on boot, so one read covers all eight."""
    ser = serial.Serial(PORT, 115200, timeout=0.3)
    try:
        ser.dtr = False
        ser.rts = True
        time.sleep(0.2)
        ser.rts = False
        time.sleep(1.2)
        buf = b""
        got = {name: {} for name in WORKLOADS}
        total = sum(len(v[1]) for v in WORKLOADS.values())
        deadline = time.time() + 300
        while time.time() < deadline:
            data = ser.read(8192)
            if not data:
                continue
            buf += data
            text = ANSI.sub(b"", buf).decode("utf-8", "replace")
            for m in re.finditer(r"sweep wl=(\w+) param=(\d+) runs=\d+ mean_ns=(\d+)", text):
                wl, p, ns = m.group(1), int(m.group(2)), int(m.group(3))
                if wl in got and p in WORKLOADS[wl][1] and p not in got[wl]:
                    got[wl][p] = ns
            if sum(len(v) for v in got.values()) >= total:
                break
        return got
    finally:
        ser.close()


def main():
    engine = sys.argv[1]
    OUT.mkdir(exist_ok=True)
    flash_firmware(engine)
    if engine == "hal":
        for rep in range(1, REPEATS + 1):
            got = collect_hal_all()
            for name, (opcode, sizes) in WORKLOADS.items():
                rows = [{"param": p, "mean_ns": v} for p, v in sorted(got[name].items())]
                miss = sorted(set(sizes) - set(got[name]))
                payload = {"workload": name, "engine": engine, "rep": rep,
                           "rows": rows, "missing": miss}
                (OUT / f"{name}_{engine}_{rep}.json").write_text(json.dumps(payload, indent=2))
                print(f"{name:11s} {engine:7s} rep={rep} n={len(rows)}/{len(sizes)} miss={miss}",
                      file=sys.stderr)
        return
    for rep in range(1, REPEATS + 1):
        for name, (opcode, sizes) in WORKLOADS.items():
            try:
                got = run_workload(engine, name, opcode, sizes)
            except RuntimeError as e:
                print(f"{name}/{engine}/{rep}: {e}", file=sys.stderr)
                continue
            miss = sorted(set(sizes) - set(got))
            payload = {
                "workload": name,
                "engine": engine,
                "rep": rep,
                "rows": [{"param": p, "mean_ns": v} for p, v in sorted(got.items())],
                "missing": miss,
            }
            (OUT / f"{name}_{engine}_{rep}.json").write_text(json.dumps(payload, indent=2))
            print(f"{name:11s} {engine:7s} rep={rep} n={len(got)}/{len(sizes)} miss={miss}",
                  file=sys.stderr)


if __name__ == "__main__":
    main()
