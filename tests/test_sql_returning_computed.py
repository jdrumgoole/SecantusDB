"""Computed expressions in a ``RETURNING`` clause.

``RETURNING`` used to accept only columns / ``*`` / jsonb paths. It now also
evaluates computed expressions (arithmetic, ``||``, function calls, ``CASE`` …)
per returned row against a scope over that row — for INSERT, UPDATE, DELETE, and
INSERT … ON CONFLICT.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    s.q = lambda sql: run_sql(s, DB, sql, session=Session(database=DB))[0]
    s.q("CREATE TABLE t (id bigint primary key, price int, qty int, name text)")
    try:
        yield s
    finally:
        s.close()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


def test_insert_returning_arithmetic_and_function(storage, session):
    res = q(
        storage,
        session,
        "INSERT INTO t (id, price, qty, name) VALUES (1, 10, 3, 'wid') "
        "RETURNING id, price * qty AS total, upper(name) AS shout",
    )
    assert res.rows == [(1, 30, "WID")]
    assert [c.name for c in res.columns] == ["id", "total", "shout"]


def test_update_returning_computed_postimage(storage, session):
    q(storage, session, "INSERT INTO t (id, price, qty) VALUES (1, 10, 3)")
    # The computed value reflects the post-update qty.
    res = q(
        storage, session, "UPDATE t SET qty = 5 WHERE id = 1 RETURNING id, price * qty AS total"
    )
    assert res.rows == [(1, 50)]


def test_delete_returning_computed(storage, session):
    q(storage, session, "INSERT INTO t (id, price, qty, name) VALUES (1, 10, 5, 'wid')")
    res = q(
        storage,
        session,
        "DELETE FROM t WHERE id = 1 RETURNING id, price * qty AS total, name || '!' AS tag",
    )
    assert res.rows == [(1, 50, "wid!")]


def test_on_conflict_returning_computed(storage, session):
    q(storage, session, "INSERT INTO t (id, price, qty) VALUES (2, 20, 2)")
    res = q(
        storage,
        session,
        "INSERT INTO t (id, price, qty) VALUES (2, 30, 4) "
        "ON CONFLICT (id) DO UPDATE SET qty = EXCLUDED.qty RETURNING id, price * qty AS total",
    )
    # qty becomes 4 (EXCLUDED); price stays 20 -> 80.
    assert res.rows == [(2, 80)]


def test_returning_case_expression(storage, session):
    res = q(
        storage,
        session,
        "INSERT INTO t (id, price, qty) VALUES (1, 10, 3) "
        "RETURNING id, CASE WHEN price > 5 THEN 'dear' ELSE 'cheap' END AS bucket",
    )
    assert res.rows == [(1, "dear")]


def test_plain_returning_still_works(storage, session):
    res = q(storage, session, "INSERT INTO t (id, price, qty) VALUES (3, 7, 7) RETURNING *")
    assert res.rows == [(3, 7, 7, None)]
