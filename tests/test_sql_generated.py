"""Generated (computed) columns — ``GENERATED ALWAYS AS (expr) STORED``.

A generated column's value is computed from the row's other columns on every
write (INSERT / UPDATE); a user value can't be supplied (``428C9``). It reflects
as ``pg_attribute.attgenerated = 's'``.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB, user="secantus")


@pytest.fixture
def storage(session, tmp_path):
    s = Storage(str(tmp_path))
    run(
        s,
        session,
        "CREATE TABLE t (id int PRIMARY KEY, w int, h int, "
        "area int GENERATED ALWAYS AS (w * h) STORED)",
    )
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def sqlstate(storage, session, sql):
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, sql)
    return ei.value.sqlstate


def test_insert_computes_generated_column(storage, session):
    run(storage, session, "INSERT INTO t (id, w, h) VALUES (1, 3, 4)")
    assert run(storage, session, "SELECT id, w, h, area FROM t").rows == [(1, 3, 4, 12)]


def test_insert_rejects_explicit_value(storage, session):
    assert sqlstate(storage, session, "INSERT INTO t (id, w, h, area) VALUES (1, 3, 4, 99)") == (
        "428C9"
    )


def test_update_recomputes_generated_column(storage, session):
    run(storage, session, "INSERT INTO t (id, w, h) VALUES (1, 3, 4)")
    run(storage, session, "UPDATE t SET w = 10 WHERE id = 1")
    assert run(storage, session, "SELECT w, h, area FROM t WHERE id = 1").rows == [(10, 4, 40)]


def test_update_of_generated_column_rejected(storage, session):
    run(storage, session, "INSERT INTO t (id, w, h) VALUES (1, 3, 4)")
    # Postgres rejects an UPDATE that targets a generated column with a value.
    assert sqlstate(storage, session, "UPDATE t SET area = 5 WHERE id = 1") == "428C9"


def test_generated_null_when_input_null(storage, session):
    run(storage, session, "INSERT INTO t (id, w, h) VALUES (1, NULL, 4)")
    assert run(storage, session, "SELECT area FROM t").rows == [(None,)]


def test_returning_includes_generated_value(storage, session):
    res = run(storage, session, "INSERT INTO t (id, w, h) VALUES (1, 5, 6) RETURNING area")
    assert res.rows == [(30,)]


def test_string_generated_column(session, tmp_path):
    s = Storage(str(tmp_path))
    run(
        s,
        session,
        "CREATE TABLE p (id int PRIMARY KEY, first text, last text, "
        "full text GENERATED ALWAYS AS (first || ' ' || last) STORED)",
    )
    run(s, session, "INSERT INTO p (id, first, last) VALUES (1, 'Ada', 'Lovelace')")
    assert run(s, session, "SELECT full FROM p").rows == [("Ada Lovelace",)]


def test_attgenerated_reflects(storage, session):
    rows = run(
        storage,
        session,
        "SELECT attname, attgenerated FROM pg_attribute WHERE attgenerated = 's'",
    ).rows
    assert rows == [("area", "s")]
