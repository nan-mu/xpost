"""Register this pixi env as a Jupyter kernelspec for an external Lab.

Usage (cwd = benchmark/)::

    pixi run kernel
"""

from __future__ import annotations

import sys
from pathlib import Path

from ipykernel.kernelspec import install

NAME = "xpost-benchmark"
DISPLAY = "xpost-benchmark (pixi)"


def main() -> None:
    root = Path(__file__).resolve().parents[1]
    path = install(
        user=True,
        kernel_name=NAME,
        display_name=DISPLAY,
        env={"PYTHONPATH": str(root)},
    )
    print(f"kernel={NAME}")
    print(f"display={DISPLAY}")
    print(f"python={sys.executable}")
    print(f"PYTHONPATH={root}")
    print(f"spec={path}")


if __name__ == "__main__":
    main()
