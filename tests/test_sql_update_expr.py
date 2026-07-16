"""``UPDATE ... SET col = <expr>`` — a per-row computed assignment (#159).

The RHS may be arithmetic, a column reference, ``||``, or a function call rather
than a literal. Each is evaluated against the *old* row (Postgres semantics: every
SET item sees the pre-image), validated, then written back. A computed
primary-key expression re-keys the row (so a PK swap works). Driven through
``run_sql`` over the real WiredTiger-backed ``Storage``.
"""

from __future__ import annotations

import pytest

from secantus.sql import SQLError, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(tmp_path, session):
    s = Storage(str(tmp_path))
    run_sql(s, DB, "CREATE TABLE t (id int PRIMARY KEY, n int, s text)", session=session)
    run_sql(s, DB, "INSERT INTO t VALUES (1, 10, 'a'), (2, 20, 'b')", session=session)
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def rows(storage, session, sql):
    return run(storage, session, sql).rows


def test_arithmetic_increment(storage, session):
    assert run(storage, session, "UPDATE t SET n = n + 1 WHERE id = 1").command_tag == "UPDATE 1"
    assert rows(storage, session, "SELECT n FROM t WHERE id = 1") == [(11,)]


def test_arithmetic_all_rows(storage, session):
    run(storage, session, "UPDATE t SET n = n * 2")
    assert rows(storage, session, "SELECT id, n FROM t ORDER BY id") == [(1, 20), (2, 40)]


def test_string_concat(storage, session):
    run(storage, session, "UPDATE t SET s = s || 'x' WHERE id = 2")
    assert rows(storage, session, "SELECT s FROM t WHERE id = 2") == [("bx",)]


def test_column_reference(storage, session):
    run(storage, session, "UPDATE t SET n = id")
    assert rows(storage, session, "SELECT id, n FROM t ORDER BY id") == [(1, 1), (2, 2)]


def test_function_call(storage, session):
    run(storage, session, "UPDATE t SET s = upper(s) WHERE id = 1")
    assert rows(storage, session, "SELECT s FROM t WHERE id = 1") == [("A",)]


def test_multiple_sets_see_old_row(storage, session):
    # Both RHS see the pre-image: n uses old n, s uses old s.
    run(storage, session, "UPDATE t SET n = n + 100, s = s || '!' WHERE id = 1")
    assert rows(storage, session, "SELECT n, s FROM t WHERE id = 1") == [(110, "a!")]


def test_swap_via_old_values(storage, session):
    run(storage, session, "UPDATE t SET n = (SELECT 0) WHERE id = 1")  # warm the path
    run(storage, session, "UPDATE t SET n = 10, s = 'a' WHERE id = 1")  # reset
    # A two-column swap reads old values for both sides.
    run(storage, session, "CREATE TABLE p (id int PRIMARY KEY, a int, b int)")
    run(storage, session, "INSERT INTO p VALUES (1, 5, 9)")
    run(storage, session, "UPDATE p SET a = b, b = a WHERE id = 1")
    assert rows(storage, session, "SELECT a, b FROM p WHERE id = 1") == [(9, 5)]


def test_returning_computed(storage, session):
    assert run(storage, session, "UPDATE t SET n = n + 1 WHERE id = 2 RETURNING id, n").rows == [
        (2, 21)
    ]


def test_literal_update_still_works(storage, session):
    # The pure-literal fast path is unchanged.
    run(storage, session, "UPDATE t SET n = 999 WHERE id = 1")
    assert rows(storage, session, "SELECT n FROM t WHERE id = 1") == [(999,)]


def test_computed_pk_swap_rekeys(storage, session):
    run(storage, session, "UPDATE t SET id = 3 - id WHERE id IN (1, 2)")
    assert rows(storage, session, "SELECT id, n FROM t ORDER BY id") == [(1, 20), (2, 10)]


def test_computed_pk_increment(storage, session):
    run(storage, session, "UPDATE t SET id = id + 10 WHERE id = 1")
    assert rows(storage, session, "SELECT id FROM t ORDER BY id") == [(2,), (11,)]


def test_computed_not_null_violation(storage, session):
    run(storage, session, "CREATE TABLE nn (id int PRIMARY KEY, v int NOT NULL)")
    run(storage, session, "INSERT INTO nn VALUES (1, 5)")
    with pytest.raises(SQLError) as ei:
        run(storage, session, "UPDATE nn SET v = NULLIF(v, 5) WHERE id = 1")
    assert ei.value.sqlstate == "23502"
    # unchanged (statement-atomic)
    assert rows(storage, session, "SELECT v FROM nn WHERE id = 1") == [(5,)]


def test_computed_check_violation(storage, session):
    run(storage, session, "CREATE TABLE ck (id int PRIMARY KEY, n int CHECK (n >= 0))")
    run(storage, session, "INSERT INTO ck VALUES (1, 5)")
    with pytest.raises(SQLError):
        run(storage, session, "UPDATE ck SET n = n - 100 WHERE id = 1")
    assert rows(storage, session, "SELECT n FROM ck WHERE id = 1") == [(5,)]
