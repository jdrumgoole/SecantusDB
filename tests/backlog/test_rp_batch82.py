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
