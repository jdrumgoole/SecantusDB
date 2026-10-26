"""Batch 82 of the Rust PG server (2026-10-25): two transactions creating
one table name (found while chasing RP-268).

PostgreSQL 15.19's second CREATE TABLE finds the name free, then waits on
the first transaction's `pg_type` index entry. If that one commits it fails
with a duplicate key in `pg_type_typname_nsp_index`; if it rolls back the
second goes on. This server failed the second at once with 40001.
Measured on PostgreSQL 15.19 with two sessions, in a block and in
autocommit.
"""

from __future__ import annotations

import threading
import time
from typing import Any

import pytest

DUPLICATE = (
    "23505",
    'duplicate key value violates unique constraint "pg_type_typname_nsp_index"',
    "Key (typname, typnamespace)=(rp82_t, 2200) already exists.",
    "pg_type_typname_nsp_index",
    "pg_type",
    "pg_catalog",
)


def _race(rust_pg: Any, end: str, autocommit: bool) -> Any:
    a = rust_pg.connect(autocommit=False)
    b = rust_pg.connect(autocommit=autocommit)
    try:
        a.execute("create table rp82_t (id serial primary key, v int)")
        result: dict[str, Any] = {}

        def second() -> None:
            started = time.monotonic()
            try:
                b.execute("create table rp82_t (id serial primary key, v int)")
                result["answer"] = "ok"
            except Exception as e:  # noqa: BLE001 - the answer is the error
                d = e.diag  # type: ignore[attr-defined]
                result["answer"] = (
                    d.sqlstate,
                    d.message_primary,
                    d.message_detail,
                    d.constraint_name,
                    d.table_name,
                    d.schema_name,
                )
            result["waited"] = time.monotonic() - started

        t = threading.Thread(target=second)
        t.start()
        time.sleep(1.0)
        getattr(a, end)()
        t.join(30)
        assert not t.is_alive()
        return result["answer"], result["waited"] >= 0.5
    finally:
        for c in (b, a):
            c.rollback()
        a.execute("drop table if exists rp82_t")
        a.commit()
        a.close()
        b.close()


@pytest.mark.backlog_fixed("RP-268")
@pytest.mark.parametrize("autocommit", [False, True], ids=["block", "autocommit"])
def test_a_second_create_table_waits_then_is_a_duplicate(rust_pg: Any, autocommit: bool) -> None:
    """PostgreSQL 15.19: the second CREATE TABLE waits, then (the first
    committed) is a duplicate key in `pg_type_typname_nsp_index`."""
    assert _race(rust_pg, "commit", autocommit) == (DUPLICATE, True)


@pytest.mark.backlog_fixed("RP-268")
@pytest.mark.parametrize("autocommit", [False, True], ids=["block", "autocommit"])
def test_a_second_create_table_waits_then_goes_on(rust_pg: Any, autocommit: bool) -> None:
    """PostgreSQL 15.19: the second CREATE TABLE waits, then (the first
    rolled back) creates the table."""
    assert _race(rust_pg, "rollback", autocommit) == ("ok", True)


@pytest.mark.backlog_fixed("RP-269")
def test_sessions_create_their_own_temp_tables_side_by_side(rust_pg: Any) -> None:
    """PostgreSQL 15.19: sixteen sessions, each forty times on a fresh
    connection creating its own TEMP table `t` and filling it in one
    implicit transaction, then updating and reading it back. Every session's temp schema
    is its own, so none of them fails and each reads only its own rows.
    This server failed some with 40001: on a fresh store, each session's
    first CREATE registered `__sql_relstats__` inside its own transaction,
    and WiredTiger refused all but one of those registrations."""
    problems: list[str] = []
    lock = threading.Lock()

    def work(w: int) -> None:
        for i in range(40):
            try:
                with rust_pg.connect() as c:
                    c.execute(
                        "create temporary table t (id text primary key, n int); "
                        f"insert into t values ('{w}-{i}-a', {w}), ('{w}-{i}-b', {w})",
                        prepare=False,
                    )
                    c.execute(
                        "update t set n = n + %s where id = %s",
                        (1, f"{w}-{i}-b"),
                        prepare=True,
                    )
                    rows = c.execute("select n from t").fetchall()
                    if sorted(r[0] for r in rows) != [w, w + 1]:
                        with lock:
                            problems.append(f"session {w} read {rows}")
            except Exception as e:  # noqa: BLE001 - the error is the answer
                with lock:
                    problems.append(f"{type(e).__name__}: {e}")

    threads = [threading.Thread(target=work, args=(w,)) for w in range(16)]
    for t in threads:
        t.start()
    for t in threads:
        t.join(300)
    assert not any(t.is_alive() for t in threads)
    assert problems == []
