"""Column DEFAULTs and ``ALTER COLUMN … TYPE`` / ``SET`` / ``DROP DEFAULT``.

A literal column DEFAULT (from ``CREATE TABLE`` or ``ALTER TABLE ALTER COLUMN
SET DEFAULT``) is applied when an INSERT omits the column. ``ALTER COLUMN …
TYPE`` retypes the column in the catalog. ``DROP DEFAULT`` clears it without
touching nullability (a case the old code got wrong).
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def rows(storage, session, sql):
    return q(storage, session, sql).rows


# -- CREATE TABLE ... DEFAULT ----------------------------------------------- #


def test_create_table_defaults_applied_on_insert(storage, session):
    q(
        storage,
        session,
        "CREATE TABLE t (id bigint primary key, n int DEFAULT 5, s text DEFAULT 'hi', "
        "active bool DEFAULT true)",
    )
    q(storage, session, "INSERT INTO t (id) VALUES (1)")  # omit defaulted cols
    q(storage, session, "INSERT INTO t (id, n) VALUES (2, 99)")  # override one
    assert rows(storage, session, "SELECT id, n, s, active FROM t ORDER BY id") == [
        (1, 5, "hi", True),
        (2, 99, "hi", True),
    ]


def test_default_null(storage, session):
    q(storage, session, "CREATE TABLE t (id bigint primary key, n int DEFAULT NULL)")
    q(storage, session, "INSERT INTO t (id) VALUES (1)")
    assert rows(storage, session, "SELECT id, n FROM t") == [(1, None)]


def test_function_default_applied(storage, session):
    # A non-literal default (now()) is evaluated per omitted row at INSERT (#166).
    import datetime

    q(storage, session, "CREATE TABLE t (id bigint primary key, at timestamptz DEFAULT now())")
    q(storage, session, "INSERT INTO t (id) VALUES (1)")
    r = rows(storage, session, "SELECT id, at FROM t")
    assert r[0][0] == 1
    assert isinstance(r[0][1], datetime.datetime)


# -- ALTER COLUMN SET / DROP DEFAULT ---------------------------------------- #


def test_alter_set_default(storage, session):
    q(storage, session, "CREATE TABLE t (id bigint primary key, n int)")
    q(storage, session, "ALTER TABLE t ALTER COLUMN n SET DEFAULT 42")
    q(storage, session, "INSERT INTO t (id) VALUES (1)")
    assert rows(storage, session, "SELECT id, n FROM t") == [(1, 42)]


def test_alter_drop_default(storage, session):
    q(storage, session, "CREATE TABLE t (id bigint primary key, n int DEFAULT 7)")
    q(storage, session, "ALTER TABLE t ALTER COLUMN n DROP DEFAULT")
    q(storage, session, "INSERT INTO t (id) VALUES (1)")
    assert rows(storage, session, "SELECT id, n FROM t") == [(1, None)]


def test_drop_default_does_not_change_nullability(storage, session):
    # Regression: DROP DEFAULT and DROP NOT NULL both parse with drop=True; the
    # old code conflated them and wrongly set the column NOT NULL.
    q(storage, session, "CREATE TABLE t (id bigint primary key, n int DEFAULT 7)")
    q(storage, session, "ALTER TABLE t ALTER COLUMN n DROP DEFAULT")
    res = q(
        storage,
        session,
        "SELECT is_nullable FROM information_schema.columns "
        "WHERE table_name = 't' AND column_name = 'n'",
    )
    assert res.rows == [("YES",)]


def test_alter_set_default_expression(storage, session):
    # A non-literal ALTER … SET DEFAULT (now()) is applied per omitted row (#166).
    import datetime

    q(storage, session, "CREATE TABLE t (id bigint primary key, at timestamptz)")
    q(storage, session, "ALTER TABLE t ALTER COLUMN at SET DEFAULT now()")
    q(storage, session, "INSERT INTO t (id) VALUES (1)")
    r = rows(storage, session, "SELECT at FROM t")
    assert isinstance(r[0][0], datetime.datetime)


# -- ALTER COLUMN TYPE ------------------------------------------------------ #


def test_alter_column_type(storage, session):
    q(storage, session, "CREATE TABLE t (id bigint primary key, n int)")
    q(storage, session, "ALTER TABLE t ALTER COLUMN n TYPE bigint")
    res = q(
        storage,
        session,
        "SELECT data_type FROM information_schema.columns "
        "WHERE table_name = 't' AND column_name = 'n'",
    )
    assert res.rows == [("bigint",)]


def test_alter_column_type_coerces_new_inserts(storage, session):
    q(storage, session, "CREATE TABLE t (id bigint primary key, n text)")
    q(storage, session, "ALTER TABLE t ALTER COLUMN n TYPE int")
    q(storage, session, "INSERT INTO t (id, n) VALUES (1, 42)")
    assert rows(storage, session, "SELECT n FROM t") == [(42,)]


# -- NOT NULL still works (unchanged) --------------------------------------- #


def test_set_and_drop_not_null_unaffected(storage, session):
    q(storage, session, "CREATE TABLE t (id bigint primary key, v int)")
    q(storage, session, "ALTER TABLE t ALTER COLUMN v SET NOT NULL")
    assert rows(
        storage,
        session,
        "SELECT is_nullable FROM information_schema.columns "
        "WHERE table_name = 't' AND column_name = 'v'",
    ) == [("NO",)]
    q(storage, session, "ALTER TABLE t ALTER COLUMN v DROP NOT NULL")
    assert rows(
        storage,
        session,
        "SELECT is_nullable FROM information_schema.columns "
        "WHERE table_name = 't' AND column_name = 'v'",
    ) == [("YES",)]


def test_drop_column_without_keyword(storage, session):
    # ``ALTER TABLE t DROP name`` (no COLUMN keyword) is valid PG — pgjdbc's
    # droppedColumns test uses it; sqlglot parses the action as a raw Command.
    q(storage, session, "CREATE TABLE dropbare (id int4, name text, colour text)")
    q(storage, session, "INSERT INTO dropbare VALUES (1, 'n', 'red')")
    assert q(storage, session, "ALTER TABLE dropbare DROP name").command_tag == "ALTER TABLE"
    assert q(storage, session, 'ALTER TABLE dropbare DROP "colour"').command_tag == "ALTER TABLE"
    assert rows(storage, session, "SELECT * FROM dropbare") == [(1,)]
    assert (
        q(storage, session, "ALTER TABLE dropbare DROP IF EXISTS nope").command_tag == "ALTER TABLE"
    )
    with pytest.raises(errors.SQLError):
        q(storage, session, "ALTER TABLE dropbare DROP nope")
