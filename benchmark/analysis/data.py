"""Read one consistent database snapshot. Never mutate the source database."""
from __future__ import annotations

import gzip
import hashlib
import json
from datetime import datetime, timezone
from pathlib import Path
import tomllib

import pandas as pd
import psycopg
from psycopg.rows import dict_row

from utils import db

ROOT = Path(__file__).resolve().parents[1]
EXPERIMENTS = ("just_drop", "firewall", "just_tx")


def read_settings(path: Path | None = None) -> dict:
    settings = tomllib.loads((path or ROOT / "report.toml").read_text())
    if settings["schema"] != "formal":
        raise ValueError("This report requires the formal schema.")
    selected = set(settings["exams"])
    if not set(EXPERIMENTS).issubset(selected) or selected - set(EXPERIMENTS) - {"exec_bench"}:
        raise ValueError("Pin the three network exams and optionally exec_bench in report.toml.")
    return settings


def encode_value(value):
    """Preserve BYTEA values as reversible hex in source snapshots."""
    if isinstance(value, (bytes, bytearray, memoryview)):
        return bytes(value).hex()
    return str(value)


def fetch_snapshot(settings: dict, output: Path | None = None) -> Path:
    """Persist source rows, query text, parameters and selection in a new directory."""
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    dest = (output or ROOT / "outputs" / "snapshots") / stamp
    tables, queries = {}, []
    kwargs = db.dsn_kwargs()
    with psycopg.connect(
        **kwargs, connect_timeout=10, row_factory=dict_row,
        options="-c default_transaction_read_only=on -c statement_timeout=120000",
    ) as connection:
        connection.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")

        def query(key: str, statement: str, params=()):
            queries.append({"key": key, "sql": statement, "parameters": list(params)})
            tables[key] = connection.execute(statement, params).fetchall()

        query("exams", "SELECT * FROM formal.exam ORDER BY started_at, id")
        for experiment in settings["exams"]:
            exam_id = settings["exams"][experiment]
            config = f"formal.{experiment}_config"
            order = "workload" if experiment == "exec_bench" else "target_pps"
            query(f"{experiment}_config", f"SELECT * FROM {config} WHERE exam_id = %s ORDER BY firmware, {order}, id", (exam_id,))
            query(f"{experiment}_data", f"SELECT d.* FROM formal.{experiment}_data d JOIN {config} c ON d.config_id = c.id WHERE c.exam_id = %s ORDER BY d.config_id, d.id", (exam_id,))
            for kind in ("counter", "agg", "gauge"):
                query(f"{experiment}_xdp_{kind}", f"SELECT p.* FROM formal.xdp_{kind} p JOIN {config} c ON p.config_id = c.id WHERE c.exam_id = %s ORDER BY p.config_id, p.name", (exam_id,))
                query(f"{experiment}_hal_{kind}", f"SELECT p.* FROM formal.{experiment}_hal_{kind} p JOIN {config} c ON p.config_id = c.id WHERE c.exam_id = %s ORDER BY p.config_id, p.name", (exam_id,))
            query(f"{experiment}_trace", f"SELECT t.* FROM formal.trace_event t JOIN {config} c ON t.config_id = c.id WHERE c.exam_id = %s ORDER BY t.config_id, t.seq", (exam_id,))
    # No connection address, username or password is exported.
    payload = json.dumps(tables, ensure_ascii=False, default=encode_value, separators=(",", ":")).encode()
    dest.mkdir(parents=True, exist_ok=False)
    with gzip.open(dest / "source.json.gz", "wb") as handle:
        handle.write(payload)
    manifest = {
        "format_version": 1, "fetched_at_utc": stamp, "schema": "formal",
        "settings": settings, "transaction": "REPEATABLE READ, READ ONLY",
        "source_sha256": hashlib.sha256(payload).hexdigest(),
        "row_counts": {key: len(value) for key, value in tables.items()}, "queries": queries,
    }
    (dest / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2))
    return dest


def load_snapshot(path: Path) -> tuple[dict, dict[str, pd.DataFrame]]:
    manifest = json.loads((path / "manifest.json").read_text())
    with gzip.open(path / "source.json.gz", "rb") as handle:
        payload = handle.read()
    if hashlib.sha256(payload).hexdigest() != manifest["source_sha256"]:
        raise ValueError("Snapshot checksum mismatch.")
    data = json.loads(payload)
    if {k: len(v) for k, v in data.items()} != manifest["row_counts"]:
        raise ValueError("Snapshot row counts do not match its manifest.")
    return manifest, {key: pd.DataFrame(rows) for key, rows in data.items()}
