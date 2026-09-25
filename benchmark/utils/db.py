"""Live Postgres access for benchmark analysis — no local result cache.

Credentials come from `benchmark/.env` (see `.env.example`). Every call opens a
fresh connection; query results are returned as DataFrames / scalars and are not
written to disk by this module.
"""

from __future__ import annotations

import os
from contextlib import contextmanager
from pathlib import Path
from typing import Any, Iterator, Mapping, Optional, Sequence, Union

import pandas as pd
import psycopg
from dotenv import load_dotenv
from psycopg import sql

ParamT = Union[Sequence[Any], Mapping[str, Any], None]

_ROOT = Path(__file__).resolve().parents[1]
_ALLOWED_SCHEMAS = frozenset({"public", "formal"})


def _load_env() -> None:
    load_dotenv(_ROOT / ".env", override=False)


def dsn_kwargs() -> dict[str, Any]:
    """Connection kwargs from env (after loading `.env`)."""
    _load_env()
    host = os.environ.get("PGHOST", "127.0.0.1")
    port = int(os.environ.get("PGPORT", "5432"))
    dbname = os.environ.get("PGDATABASE", "postgres")
    user = os.environ.get("PGUSER", "")
    password = os.environ.get("PGPASSWORD", "")
    if not user:
        raise RuntimeError("PGUSER missing — copy benchmark/.env.example → .env")
    return {
        "host": host,
        "port": port,
        "dbname": dbname,
        "user": user,
        "password": password,
    }


def schema() -> str:
    _load_env()
    s = os.environ.get("PGSCHEMA", "public")
    if s not in _ALLOWED_SCHEMAS:
        raise RuntimeError(f"PGSCHEMA must be one of {_ALLOWED_SCHEMAS}, got {s!r}")
    return s


def dsn_summary() -> str:
    """Safe one-line summary (no password) for notebook headers."""
    kw = dsn_kwargs()
    return f"{kw['user']}@{kw['host']}:{kw['port']}/{kw['dbname']} schema={schema()}"


@contextmanager
def connect(*, set_search_path: bool = True) -> Iterator[psycopg.Connection]:
    """Open a short-lived connection (no pooling / no result cache)."""
    kw = dsn_kwargs()
    with psycopg.connect(**kw) as conn:
        if set_search_path:
            conn.execute(
                sql.SQL("SET search_path TO {}, public").format(sql.Identifier(schema()))
            )
        yield conn


def execute(sql_text: str, params: ParamT = None) -> None:
    """Run a statement (DDL/DML). Commits on success."""
    with connect() as conn:
        conn.execute(sql_text, params)
        conn.commit()


def read_sql(sql_text: str, params: ParamT = None) -> pd.DataFrame:
    """Run a SELECT and return a DataFrame. No disk cache."""
    with connect() as conn:
        with conn.cursor() as cur:
            cur.execute(sql_text, params)
            cols = [d.name for d in (cur.description or [])]
            rows = cur.fetchall()
    return pd.DataFrame(rows, columns=cols)


def read_sql_one(sql_text: str, params: ParamT = None) -> Optional[tuple]:
    with connect() as conn:
        with conn.cursor() as cur:
            cur.execute(sql_text, params)
            return cur.fetchone()
