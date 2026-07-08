"""Enum-aware ORDER BY across the pipeline paths — GROUP BY, DISTINCT, JOIN,
JOIN+GROUP BY, and the evaluated (computed-column) path all sort an enum column by
its declared label order, not lexically.

The single-table pushdown case is covered in ``test_sql_alter_type.py``; this file
pins the pipeline / evaluated planners that #80 left sorting lexically.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB, user="secantus")


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


@pytest.fixture
def data(storage, session):
    # Declared order sad < ok < happy; lexical order would be happy < ok < sad.
    run(storage, session, "CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy')")
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, m mood, n int)")
    run(storage, session, "INSERT INTO t VALUES (1,'happy',5),(2,'sad',3),(3,'ok',7)")
    run(storage, session, "CREATE TABLE u (id int PRIMARY KEY, t_id int)")
    run(storage, session, "INSERT INTO u VALUES (10,1),(20,2),(30,3),(40,1)")
    return storage


def test_group_by_enum_declared_order(data, session):
    rows = run(data, session, "SELECT m, count(*) FROM t GROUP BY m ORDER BY m").rows
    assert [r[0] for r in rows] == ["sad", "ok", "happy"]


def test_group_by_enum_desc(data, session):
    rows = run(data, session, "SELECT m FROM t GROUP BY m ORDER BY m DESC").rows
    assert [r[0] for r in rows] == ["happy", "ok", "sad"]


def test_distinct_enum_declared_order(data, session):
    rows = run(data, session, "SELECT DISTINCT m FROM t ORDER BY m").rows
    assert [r[0] for r in rows] == ["sad", "ok", "happy"]


def test_evaluated_computed_column_enum_order(data, session):
    # A computed SELECT-list expression routes through the evaluated planner.
    rows = run(data, session, "SELECT n * 2 AS d, m FROM t ORDER BY m").rows
    assert [r[1] for r in rows] == ["sad", "ok", "happy"]


def test_join_enum_order(data, session):
    rows = run(
        data,
        session,
        "SELECT u.id, t.m FROM u JOIN t ON u.t_id = t.id ORDER BY t.m, u.id",
    ).rows
    assert [r[1] for r in rows] == ["sad", "ok", "happy", "happy"]
    assert rows == [(20, "sad"), (30, "ok"), (10, "happy"), (40, "happy")]


def test_join_group_by_enum_order(data, session):
    rows = run(
        data,
        session,
        "SELECT t.m, count(*) FROM u JOIN t ON u.t_id = t.id GROUP BY t.m ORDER BY t.m",
    ).rows
    assert rows == [("sad", 1), ("ok", 1), ("happy", 2)]


def test_join_evaluated_enum_order(data, session):
    # A scalar expression in the join SELECT list uses the evaluated join planner.
    rows = run(
        data,
        session,
        "SELECT upper(t.m::text) AS mm, t.m FROM u JOIN t ON u.t_id = t.id ORDER BY t.m",
    ).rows
    assert [r[1] for r in rows] == ["sad", "ok", "happy", "happy"]


def test_pipeline_enum_order_reflects_added_value(data, session):
    # A label added mid-order via ALTER TYPE sorts in its declared position.
    run(data, session, "ALTER TYPE mood ADD VALUE 'meh' AFTER 'ok'")
    run(data, session, "INSERT INTO t VALUES (4,'meh',1)")
    rows = run(data, session, "SELECT m, count(*) FROM t GROUP BY m ORDER BY m").rows
    assert [r[0] for r in rows] == ["sad", "ok", "meh", "happy"]
