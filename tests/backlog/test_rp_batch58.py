"""Batch 58 of the Rust PG server (2026-10-24): a heap kept across a
restart (RP-193; decided by Joe: a checkpointer and a reconcile) and each
database's tables a heap of their own (RP-233, found building it).

The literals are PostgreSQL 15.19's answers, measured on a private,
freshly initdb'd cluster with the same code.
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


def two_databases(dsn: str) -> list[Any]:
    """The first table of each of two new databases, three rows each."""
    out: list[Any] = []
    with psycopg.connect(dsn + " dbname=postgres", autocommit=True) as admin:
        for db in ("rp58_x", "rp58_y"):
            admin.execute(f"drop database if exists {db}")
            admin.execute(f"create database {db}")
        try:
            for db in ("rp58_x", "rp58_y"):
                with psycopg.connect(f"{dsn} dbname={db}", autocommit=True) as c:
                    c.execute("create table t (x int)")
                    c.execute("insert into t select generate_series(1, 3)")
                    out.append([str(r[0]) for r in c.execute("select ctid from t order by x")])
            # A database made again under a dropped one's name starts afresh.
            admin.execute("drop database rp58_x")
            admin.execute("create database rp58_x")
            with psycopg.connect(f"{dsn} dbname=rp58_x", autocommit=True) as c:
                c.execute("create table t (x int)")
                c.execute("insert into t values (1)")
                out.append([str(r[0]) for r in c.execute("select ctid from t")])
        finally:
            for db in ("rp58_x", "rp58_y"):
                admin.execute(f"drop database if exists {db}")
    return out


@pytest.mark.backlog_fixed("RP-233")
def test_two_databases_tables_have_their_own_heaps(rust_pg: Any) -> None:
    assert two_databases(_dsn(rust_pg)) == [
        ["(0,1)", "(0,2)", "(0,3)"],
        ["(0,1)", "(0,2)", "(0,3)"],
        ["(0,1)"],
    ]


if __name__ == "__main__":
    import sys

    print(two_databases(sys.argv[1]))
