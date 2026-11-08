#!/usr/bin/env python3
"""Run every sweepable workload on both XDP engines, repeated for error bars.

Repeating matters for the figure: one sweep gives a line, five give a spread, and
the spread is what says whether the fit is stable or an artefact of one boot.

The HAL arm is not here. Its work size is fixed by the carrier map it reads, so a
scale sweep does not apply to it; it appears only in the per-workload comparison.

Writes one JSON per (workload, engine) under `/tmp/sweeps/`.
"""

import json
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).parent
OUT = Path("/tmp/sweeps")
REPEATS = 5
ENGINES = ["aot", "interp"]
WORKLOADS = ["buf", "fib"]

FIRMWARE = {
    "aot": "/tmp/exec_aot_diag",
    "interp": "/tmp/exec_interp_diag",
}
PORT = "/dev/cu.usbmodem21101"


def main():
    OUT.mkdir(exist_ok=True)
    for engine in ENGINES:
        res = subprocess.run(
            ["espflash", "flash", "--port", PORT, FIRMWARE[engine]],
            capture_output=True,
        )
        if res.returncode != 0:
            raise SystemExit(
                f"flash {engine} failed: {res.stderr.decode('utf-8', 'replace')[:300]}"
            )
        for workload in WORKLOADS:
            for rep in range(1, REPEATS + 1):
                res = subprocess.run(
                    [sys.executable, str(HERE / "scale_sweep.py"), workload, engine],
                    capture_output=True,
                )
                if res.returncode != 0:
                    print(
                        f"{workload}/{engine}/{rep}: {res.stderr.decode('utf-8', 'replace')[:200]}",
                        file=sys.stderr,
                    )
                    continue
                data = json.loads(res.stdout)
                data["rep"] = rep
                path = OUT / f"{workload}_{engine}_{rep}.json"
                path.write_text(json.dumps(data, indent=2))
                n = len(data["rows"])
                miss = data["missing"]
                print(f"{workload:5s} {engine:7s} rep={rep} rows={n} missing={miss}",
                      file=sys.stderr)


if __name__ == "__main__":
    main()
