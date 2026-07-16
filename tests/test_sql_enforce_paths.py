"""Constraint enforcement across the remaining write paths — ``MERGE`` and
``INSERT … ON CONFLICT`` secondary constraints.

The per-statement INSERT/UPDATE/DELETE paths already enforce NOT NULL / CHECK /
UNIQUE / FK; this pins that a MERGE's INSERT/UPDATE/DELETE actions and an ON
CONFLICT that touches a constraint *other than its arbiter target* are gated the
same way.
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
def storage(session, tmp_path):
    s = Storage(str(tmp_path))
    run_sql(s, DB, "CREATE TABLE users (id bigint primary key)", session=session)
    run_sql(s, DB, "INSERT INTO users (id) VALUES (1)", session=session)
    run_sql(
        s,
        DB,
        "CREATE TABLE t (id bigint primary key, email text UNIQUE, "
        "uid bigint REFERENCES users(id), n int CHECK (n >= 0))",
        session=session,
    )
    run_sql(s, DB, "INSERT INTO t (id, email, uid, n) VALUES (1, 'a', 1, 5)", session=session)
    run_sql(
        s,
        DB,
        "CREATE TABLE src (id bigint primary key, email text, uid bigint, n int)",
        session=session,
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


def rows(storage, session, sql):
    return run(storage, session, sql).rows


_MERGE_INSERT = (
    "MERGE INTO t USING src ON t.id = src.id WHEN NOT MATCHED THEN "
    "INSERT (id, email, uid, n) VALUES (src.id, src.email, src.uid, src.n)"
)


def test_merge_insert_enforces_unique(storage, session):
    run(storage, session, "INSERT INTO src (id, email, uid, n) VALUES (2, 'a', 1, 3)")
    assert sqlstate(storage, session, _MERGE_INSERT) == "23505"


def test_merge_insert_enforces_fk(storage, session):
    run(storage, session, "INSERT INTO src (id, email, uid, n) VALUES (2, 'b', 99, 3)")
    assert sqlstate(storage, session, _MERGE_INSERT) == "23503"


def test_merge_insert_enforces_check(storage, session):
    run(storage, session, "INSERT INTO src (id, email, uid, n) VALUES (2, 'b', 1, -9)")
    assert sqlstate(storage, session, _MERGE_INSERT) == "23514"


def test_merge_insert_valid(storage, session):
    run(storage, session, "INSERT INTO src (id, email, uid, n) VALUES (2, 'b', 1, 3)")
    run(storage, session, _MERGE_INSERT)
    assert rows(storage, session, "SELECT count(*) FROM t") == [(2,)]


def test_merge_update_enforces_check(storage, session):
    run(storage, session, "INSERT INTO src (id, email, uid, n) VALUES (1, 'a', 1, -1)")
    stmt = "MERGE INTO t USING src ON t.id = src.id WHEN MATCHED THEN UPDATE SET n = src.n"
    assert sqlstate(storage, session, stmt) == "23514"
    assert rows(storage, session, "SELECT n FROM t WHERE id = 1") == [(5,)]


def test_merge_update_enforces_unique(storage, session):
    run(storage, session, "INSERT INTO t (id, email, uid, n) VALUES (2, 'b', 1, 0)")
    run(storage, session, "INSERT INTO src (id, email, uid, n) VALUES (2, 'a', 1, 0)")
    stmt = "MERGE INTO t USING src ON t.id = src.id WHEN MATCHED THEN UPDATE SET email = src.email"
    assert sqlstate(storage, session, stmt) == "23505"
    assert rows(storage, session, "SELECT email FROM t WHERE id = 2") == [("b",)]


def test_merge_delete_enforces_fk_restrict(storage, session):
    run(storage, session, "CREATE TABLE dept (id bigint primary key)")
    run(storage, session, "INSERT INTO dept (id) VALUES (7)")
    run(
        storage, session, "CREATE TABLE emp (id bigint primary key, did bigint REFERENCES dept(id))"
    )
    run(storage, session, "INSERT INTO emp (id, did) VALUES (1, 7)")
    run(storage, session, "CREATE TABLE dsrc (id bigint primary key)")
    run(storage, session, "INSERT INTO dsrc (id) VALUES (7)")
    stmt = "MERGE INTO dept USING dsrc ON dept.id = dsrc.id WHEN MATCHED THEN DELETE"
    assert sqlstate(storage, session, stmt) == "23503"
    assert rows(storage, session, "SELECT count(*) FROM dept") == [(1,)]


def test_merge_delete_cascades(storage, session):
    run(storage, session, "CREATE TABLE dept (id bigint primary key)")
    run(storage, session, "INSERT INTO dept (id) VALUES (7)")
    run(
        storage,
        session,
        "CREATE TABLE emp (id bigint primary key, "
        "did bigint REFERENCES dept(id) ON DELETE CASCADE)",
    )
    run(storage, session, "INSERT INTO emp (id, did) VALUES (1, 7)")
    run(storage, session, "CREATE TABLE dsrc (id bigint primary key)")
    run(storage, session, "INSERT INTO dsrc (id) VALUES (7)")
    run(
        storage, session, "MERGE INTO dept USING dsrc ON dept.id = dsrc.id WHEN MATCHED THEN DELETE"
    )
    assert rows(storage, session, "SELECT count(*) FROM dept") == [(0,)]
    assert rows(storage, session, "SELECT count(*) FROM emp") == [(0,)]


def test_on_conflict_do_update_secondary_unique(storage, session):
    run(storage, session, "CREATE TABLE c (id bigint primary key, u text UNIQUE)")
    run(storage, session, "INSERT INTO c (id, u) VALUES (1, 'x'), (2, 'y')")
    stmt = "INSERT INTO c (id, u) VALUES (2, 'z') ON CONFLICT (id) DO UPDATE SET u = 'x'"
    assert sqlstate(storage, session, stmt) == "23505"
    assert rows(storage, session, "SELECT u FROM c WHERE id = 2") == [("y",)]


def test_on_conflict_insert_secondary_unique(storage, session):
    # DO NOTHING on the PK arbiter, but the fresh row still collides on `u`.
    run(storage, session, "CREATE TABLE c (id bigint primary key, u text UNIQUE)")
    run(storage, session, "INSERT INTO c (id, u) VALUES (1, 'x')")
    stmt = "INSERT INTO c (id, u) VALUES (2, 'x') ON CONFLICT (id) DO NOTHING"
    assert sqlstate(storage, session, stmt) == "23505"


def test_on_conflict_do_update_enforces_fk(storage, session):
    run(storage, session, "INSERT INTO t (id, email, uid, n) VALUES (2, 'b', 1, 0)")
    stmt = (
        "INSERT INTO t (id, email, uid, n) VALUES (2, 'c', 1, 0) "
        "ON CONFLICT (id) DO UPDATE SET uid = 99"
    )
    assert sqlstate(storage, session, stmt) == "23503"
