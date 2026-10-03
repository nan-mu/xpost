"""Ingest probe CSV + sidecar summary into experiment_* (pilot runs).

Usage:
  python -m utils.ingest_pps /tmp/hal_pps.csv /tmp/hal_pps_summary.json
  python -m utils.ingest_pps /tmp/xdp_pps.csv /tmp/xdp_pps_summary.json [--pair-id N]
"""

from __future__ import annotations

import argparse
import json
import uuid
from pathlib import Path

import pandas as pd

from utils.db import connect, dsn_summary
from utils.ensure_schema import main as ensure_schema


def _ns(t_s: float) -> int:
    return int(round(float(t_s) * 1_000_000_000))


def ingest_csv(
    conn,
    csv_path: Path,
    summary_path: Path,
    *,
    pair_id: int = 0,
    replace_note: str | None = None,
) -> list[uuid.UUID]:
    """Load one probe CSV+JSON into experiment_runs / counter_samples as pilot.

    Each CSV round becomes one run_id. Re-import with the same source_note
    deletes prior pilot rows that share that note before insert.
    """
    summary = json.loads(summary_path.read_text())
    samples = pd.read_csv(csv_path)
    path = str(summary.get("path") or samples["path"].iloc[0]).lower()
    if path not in ("hal", "xdp"):
        raise ValueError(f"variant must be hal|xdp, got {path!r}")

    note = replace_note or f"pilot from {csv_path.name}"
    with conn.cursor() as cur:
        cur.execute(
            """
            DELETE FROM experiment_runs
            WHERE run_kind = 'pilot' AND source_note = %s
            """,
            (note,),
        )

        run_ids: list[uuid.UUID] = []
        rounds = sorted(int(r) for r in samples["round"].unique())
        for rnd in rounds:
            sub = samples[samples["round"] == rnd].sort_values("t_s")
            if sub.empty:
                continue
            detail = next(
                (d for d in summary.get("rounds_detail", []) if int(d["round"]) == rnd),
                {},
            )
            run_id = uuid.uuid4()
            cur.execute(
                """
                INSERT INTO experiment_runs (
                    run_id, pair_id, run_kind, variant,
                    sender_target_pps, udp_payload_bytes,
                    window_secs, sample_interval_ms,
                    started_at_utc, status, status_reason, source_note,
                    firmware_sha256
                ) VALUES (
                    %s, %s, 'pilot', %s,
                    %s, 8,
                    %s, %s,
                    now(), 'complete', %s, %s,
                    %s
                )
                """,
                (
                    run_id,
                    pair_id if pair_id else rnd,
                    path,
                    int(summary.get("sender_target_pps") or 0) or None,
                    float(summary.get("window_secs") or 0) or None,
                    int(summary.get("sample_interval_ms") or 0) or None,
                    f"round={rnd} overall_pps={detail.get('overall_pps')}",
                    note,
                    summary.get("firmware_sha256"),
                ),
            )
            t0 = float(sub["t_s"].iloc[0])
            for _, row in sub.iterrows():
                cur.execute(
                    """
                    INSERT INTO counter_samples (run_id, time_ns, counter_value)
                    VALUES (%s, %s, %s)
                    ON CONFLICT (run_id, time_ns) DO UPDATE
                      SET counter_value = EXCLUDED.counter_value
                    """,
                    (run_id, _ns(float(row["t_s"]) - t0), int(row["drop"])),
                )
            run_ids.append(run_id)
    return run_ids


def main(argv: list[str] | None = None) -> None:
    p = argparse.ArgumentParser(description="Ingest just-drop-pps CSV into experiment_*")
    p.add_argument("csv", type=Path, help="probe CSV path")
    p.add_argument("summary", type=Path, help="sidecar *_summary.json path")
    p.add_argument("--pair-id", type=int, default=0, help="pair_id (default: use round)")
    p.add_argument(
        "--note",
        default=None,
        help="source_note for replace; default 'pilot from <csv name>'",
    )
    args = p.parse_args(argv)

    ensure_schema()
    print(f"ingest-pps → {dsn_summary()}")
    if not args.csv.exists():
        raise SystemExit(f"missing csv {args.csv}")
    if not args.summary.exists():
        raise SystemExit(f"missing summary {args.summary}")

    with connect() as conn:
        ids = ingest_csv(
            conn,
            args.csv,
            args.summary,
            pair_id=args.pair_id,
            replace_note=args.note,
        )
        conn.commit()
    for rid in ids:
        print(f"  run_id={rid}")
    print(f"ok: {len(ids)} pilot run(s)")


if __name__ == "__main__":
    main()
