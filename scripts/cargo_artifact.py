#!/usr/bin/env python3
"""Build a package and print where its executable landed.

The problem this solves: `CARGO_TARGET_DIR` is often set in the environment, so
`target/<triple>/release/<name>` is not where the artifact appears. A caller that
guesses the path keeps running a stale binary, and nothing fails loudly. That
mistake is expensive because the stale binary runs and answers with old
behaviour, so every conclusion drawn from it is wrong in a plausible way.

So the path is asked for, never assumed. `cargo metadata` reports the target
directory the build will actually use, and the artifact name comes from the
package manifest.

Usage:
    cargo_artifact.py --manifest xpost-agent/Cargo.toml --bin xpost-agent
    cargo_artifact.py --manifest examples/exec_bench/Cargo.toml --bin exec_bench --features interp
    cargo_artifact.py --manifest xpost-agent/Cargo.toml --bin xpost-agent --print-only

The script builds by default (release), then prints the absolute path on stdout.
Progress and errors go to stderr, so `$(cargo_artifact.py ...)` captures only the
path.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path


def metadata(manifest: Path) -> dict:
    out = subprocess.run(
        [
            "cargo",
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--manifest-path",
            str(manifest),
        ],
        capture_output=True,
        text=True,
        check=True,
    )
    return json.loads(out.stdout)


def package_of(meta: dict, manifest: Path) -> dict:
    manifest = manifest.resolve()
    for pkg in meta["packages"]:
        if Path(pkg["manifest_path"]).resolve() == manifest:
            return pkg
    raise SystemExit(f"{manifest}: package not found in metadata")


def target_triple(meta: dict, manifest: Path) -> str:
    """The triple this package builds for.

    Precedence matches cargo: an explicit `--target`, then the environment, then
    the nearest `.cargo/config.toml` `build.target`, then the host. The nearest
    config is found by walking up from the manifest, which is what cargo does.
    """
    explicit = os.environ.get("CARGO_BUILD_TARGET")
    if explicit:
        return explicit
    here = manifest.resolve().parent
    for parent in [here, *here.parents]:
        cfg = parent / ".cargo" / "config.toml"
        if not cfg.is_file():
            continue
        text = cfg.read_text()
        for line in text.splitlines():
            stripped = line.strip()
            if stripped.startswith("target") and "=" in stripped:
                value = stripped.split("=", 1)[1].strip().strip('"').strip("'")
                if value and not value.startswith("["):
                    return value
        break
    # Host triple: cargo reports it in the rustc output, but `rustc -vV` is the
    # documented source and needs no build.
    out = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True)
    for line in out.stdout.splitlines():
        if line.startswith("host:"):
            return line.split(":", 1)[1].strip()
    raise SystemExit("cannot determine host triple")


def artifact_name(pkg: dict, bin_name: str) -> str:
    for target in pkg["targets"]:
        if target["name"] == bin_name and "bin" in target["kind"]:
            return target["name"]
    raise SystemExit(f"{pkg['name']}: no binary target named {bin_name}")


def main() -> None:
    p = argparse.ArgumentParser(prog="cargo_artifact")
    p.add_argument("--manifest", required=True, type=Path)
    p.add_argument("--bin", required=True)
    p.add_argument("--features", default=None)
    p.add_argument("--profile", default="release")
    p.add_argument(
        "--print-only",
        action="store_true",
        help="skip the build and print where the artifact would be",
    )
    args = p.parse_args()

    manifest = args.manifest.resolve()
    meta = metadata(manifest)
    pkg = package_of(meta, manifest)
    name = artifact_name(pkg, args.bin)
    triple = target_triple(meta, manifest)
    target_dir = Path(meta["target_directory"])
    path = target_dir / triple / args.profile / name

    if not args.print_only:
        cmd = [
            "cargo",
            "build",
            "--manifest-path",
            str(manifest),
            "--bin",
            name,
            "--target",
            triple,
        ]
        if args.profile != "release":
            cmd += ["--profile", args.profile]
        else:
            cmd += ["--release"]
        if args.features:
            cmd += ["--features", args.features]
        sys.stderr.write("cargo_artifact: " + " ".join(cmd) + "\n")
        subprocess.run(cmd, check=True)
        if not path.is_file():
            raise SystemExit(
                f"{path} does not exist after a successful build; "
                f"target dir is {target_dir}, triple {triple}"
            )

    print(path)


if __name__ == "__main__":
    main()
