"""One-shot: load legacy CSV/JSON under results/ into just_drop_pps_* (Postgres).

Notebooks must read via utils.db — this script only seeds the DB.
"""

from __future__ import annotations

import json
from pathlib import Path

import pandas as pd

from utils.db import connect, dsn_summary
from utils.ensure_schema import main as ensure_schema

_ROOT = Path(__file__).resolve().parents[1]
_RESULTS = _ROOT / "results"


def _ingest_path(conn, path: str, csv_path: Path, summary_path: Path) -> int:
    summary = json.loads(summary_path.read_text())
    samples = pd.read_csv(csv_path)
    samples = samples[samples["path"] == path]

    with conn.cursor() as cur:
        cur.execute(
            """
            INSERT INTO just_drop_pps_run
                (path, window_secs, sample_interval_ms, note)
            VALUES (%s, %s, %s, %s)
            RETURNING id
            """,
            (
                path,
                int(summary["window_secs"]),
                int(summary["sample_interval_ms"]),
                f"seed from {csv_path.name}",
            ),
        )
        run_id = cur.fetchone()[0]

        for detail in summary["rounds_detail"]:
            rnd = int(detail["round"])
            cur.execute(
                """
                INSERT INTO just_drop_pps_round (
                    run_id, round, overall_pps, interval_pps_mean, interval_pps_std,
                    host_pps, drop_delta, net_rx_delta, elapsed_secs
                ) VALUES (%s,%s,%s,%s,%s,%s,%s,%s,%s)
                RETURNING id
                """,
                (
                    run_id,
                    rnd,
                    float(detail["overall_pps"]),
                    float(detail["interval_pps_mean"]),
                    float(detail["interval_pps_std"]),
                    float(detail["host_pps"]),
                    int(detail["drop_delta"]),
                    int(detail["net_rx_delta"]),
                    float(detail["elapsed_secs"]),
                ),
            )
            round_id = cur.fetchone()[0]
            sub = samples[samples["round"] == rnd]
            for _, row in sub.iterrows():
                cur.execute(
                    """
                    INSERT INTO just_drop_pps_sample (
                        round_id, t_s, drop_count, net_rx, host_tx_end,
                        interval_pps, overall_pps_so_far
                    ) VALUES (%s,%s,%s,%s,%s,%s,%s)
                    """,
                    (
                        round_id,
                        float(row["t_s"]),
                        int(row["drop"]),
                        int(row["net_rx"]),
                        int(row["host_tx_end"]),
                        float(row["interval_pps"]),
                        float(row["overall_pps_so_far"]),
                    ),
                )
    return run_id


def main() -> None:
    ensure_schema()
    print(f"ingest-pps → {dsn_summary()}")
    pairs = [
        ("hal", _RESULTS / "hal_pps.csv", _RESULTS / "hal_pps_summary.json"),
        ("xdp", _RESULTS / "xdp_pps.csv", _RESULTS / "xdp_pps_summary.json"),
    ]
    with connect() as conn:
        # Idempotent-ish: delete prior seed runs for these paths with same note prefix
        conn.execute(
            "DELETE FROM just_drop_pps_run WHERE note LIKE 'seed from %_pps.csv'"
        )
        for path, csv_p, sum_p in pairs:
            if not csv_p.exists() or not sum_p.exists():
                raise SystemExit(f"missing {csv_p} or {sum_p}")
            run_id = _ingest_path(conn, path, csv_p, sum_p)
            print(f"  path={path} run_id={run_id}")
        conn.commit()
    print("ok")


if __name__ == "__main__":
    main()
