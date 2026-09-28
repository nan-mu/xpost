"""CREATE IF NOT EXISTS for just_tx_* / just_drop_* (loads xpost-test/sql/*.sql)."""

from __future__ import annotations

from pathlib import Path

from utils.db import connect, dsn_summary, schema

# xpost/benchmark/utils/this → xpost/xpost-test/sql
_SQL_DIR = Path(__file__).resolve().parents[2] / "xpost-test" / "sql"
_SQL_FILES = ("just_tx.sql", "just_drop.sql")


def main() -> None:
    print(f"ensure-schema → {dsn_summary()}")
    with connect() as conn:
        sch = schema()
        if sch != "public":
            conn.execute(f"CREATE SCHEMA IF NOT EXISTS {sch}")
        for name in _SQL_FILES:
            path = _SQL_DIR / name
            text = path.read_text(encoding="utf-8")
            print(f"  apply {path.relative_to(_SQL_DIR.parent.parent)}")
            conn.execute(text)
        conn.commit()
    print("ok: just_tx_* / just_drop_*")


if __name__ == "__main__":
    main()
