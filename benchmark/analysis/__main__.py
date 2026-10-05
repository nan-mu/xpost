"""Run from benchmark/: python -m analysis refresh | build --snapshot PATH."""
import argparse
from pathlib import Path

from .data import fetch_snapshot, read_settings


def main():
    parser=argparse.ArgumentParser(description="生成 report.md 使用的图片和数据，不改写正文")
    parser.add_argument("command",choices=["refresh","build"])
    parser.add_argument("--snapshot",type=Path,help="离线重建使用的快照目录")
    parser.add_argument("--config",type=Path,help="指定批次配置（默认 report.toml）")
    args=parser.parse_args()
    if args.command=="build" and args.snapshot is None:
        parser.error("build requires --snapshot; use refresh for a read-only database export")
    if args.command=="refresh" and args.snapshot is not None:
        parser.error("refresh cannot use --snapshot")
    snapshot=args.snapshot or fetch_snapshot(read_settings(args.config))
    print(f"Snapshot: {snapshot}",flush=True)
    from .report import build
    print(f"Figures: {build(snapshot)}",flush=True)


if __name__=="__main__":
    main()
