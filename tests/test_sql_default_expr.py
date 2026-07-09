"""Non-literal column DEFAULT (#166) — ``now()`` / ``gen_random_uuid()`` /
``CURRENT_DATE`` / arithmetic expressions, evaluated per omitted row at INSERT.
Driven through ``run_sql`` over the real WiredTiger-backed ``Storage``.
"""

from __future__ import annotations

import datetime

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
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def rows(storage, session, sql):
    return run(storage, session, sql).rows


def test_now_and_current_date(storage, session):
    run(
        storage,
        session,
        "CREATE TABLE t (id int primary key, ts timestamptz DEFAULT now(), "
        "d date DEFAULT CURRENT_DATE)",
    )
    run(storage, session, "INSERT INTO t (id) VALUES (1)")
    r = rows(storage, session, "SELECT ts, d FROM t")[0]
    assert isinstance(r[0], datetime.datetime)
    # the embedded run_sql path renders a date column as an ISO string
    assert r[1] is not None and str(r[1]).startswith(str(datetime.date.today().year))


def test_arithmetic_and_string_default(storage, session):
    run(
        storage,
        session,
        "CREATE TABLE t (id int primary key, n int DEFAULT 3 + 4, label text DEFAULT 'hi')",
    )
    run(storage, session, "INSERT INTO t (id) VALUES (1)")
    assert rows(storage, session, "SELECT n, label FROM t") == [(7, "hi")]


def test_gen_random_uuid_is_per_row(storage, session):
    run(storage, session, "CREATE TABLE t (id int primary key, u uuid DEFAULT gen_random_uuid())")
    run(storage, session, "INSERT INTO t (id) VALUES (1), (2), (3)")
    us = [r[0] for r in rows(storage, session, "SELECT u FROM t ORDER BY id")]
    assert len(set(us)) == 3  # distinct per row
    assert all(len(str(u)) == 36 for u in us)


def test_supplied_value_overrides_default(storage, session):
    run(storage, session, "CREATE TABLE t (id int primary key, n int DEFAULT 99)")
    run(storage, session, "INSERT INTO t (id, n) VALUES (1, 5)")
    run(storage, session, "INSERT INTO t (id) VALUES (2)")
    assert rows(storage, session, "SELECT id, n FROM t ORDER BY id") == [(1, 5), (2, 99)]


def test_insert_select_applies_expr_default(storage, session):
    run(storage, session, "CREATE TABLE src (id int primary key)")
    run(storage, session, "INSERT INTO src VALUES (1), (2)")
    run(storage, session, "CREATE TABLE t (id int primary key, ts timestamptz DEFAULT now())")
    run(storage, session, "INSERT INTO t (id) SELECT id FROM src")
    r = rows(storage, session, "SELECT id, ts FROM t ORDER BY id")
    assert [x[0] for x in r] == [1, 2]
    assert all(isinstance(x[1], datetime.datetime) for x in r)


def test_column_default_reflected(storage, session):
    run(
        storage,
        session,
        "CREATE TABLE t (id serial primary key, n int DEFAULT 5, ts timestamptz DEFAULT now())",
    )
    got = dict(
        rows(
            storage,
            session,
            "SELECT column_name, column_default FROM information_schema.columns "
            "WHERE table_name = 't' ORDER BY ordinal_position",
        )
    )
    assert got["n"] == "5"
    assert "nextval" in got["id"]
    assert got["ts"] is not None  # rendered expression text


def test_expr_default_survives_reopen(tmp_path, session):
    path = str(tmp_path)
    s = Storage(path, durable=True)
    try:
        run(s, session, "CREATE TABLE t (id int primary key, n int DEFAULT 2 * 5)")
    finally:
        s.close()
    s2 = Storage(path, durable=True)
    try:
        run(s2, session, "INSERT INTO t (id) VALUES (1)")
        assert rows(s2, session, "SELECT n FROM t") == [(10,)]
    finally:
        s2.close()


def test_pg_attrdef_reflects_defaults(storage, session):
    # #171: pg_catalog.pg_attrdef emits one row per column with a DEFAULT (adbin =
    # the rendered default text), matching information_schema.columns.column_default.
    run(
        storage,
        session,
        "CREATE TABLE t (id int primary key, n int DEFAULT 5, s text DEFAULT 'hi', "
        "ts timestamptz DEFAULT now())",
    )
    ad = rows(storage, session, "SELECT adnum, adbin FROM pg_catalog.pg_attrdef ORDER BY adnum")
    # id (attnum 1) has no default; n/s/ts do.
    assert ad == [(2, "5"), (3, "'hi'::text"), (4, "CURRENT_TIMESTAMP")]
