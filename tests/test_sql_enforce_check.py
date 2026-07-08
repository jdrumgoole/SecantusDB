"""Enforcement of NOT NULL (``23502``) and CHECK (``23514``) on write.

Unlike the reflection-only constraint modeling, these are *enforced*: an INSERT
or UPDATE that would leave a row violating a declared NOT NULL / CHECK is
rejected and the table is left unchanged. A CHECK whose predicate evaluates to
NULL (unknown) passes, matching Postgres.
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
    run_sql(
        s,
        DB,
        "CREATE TABLE t (id bigint primary key, name text NOT NULL, age int CHECK (age >= 0))",
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


def test_insert_not_null_explicit(storage, session):
    assert (
        sqlstate(storage, session, "INSERT INTO t (id, name, age) VALUES (1, NULL, 5)") == "23502"
    )


def test_insert_not_null_omitted(storage, session):
    # Omitting a NOT NULL column is a null value too.
    assert sqlstate(storage, session, "INSERT INTO t (id, age) VALUES (1, 5)") == "23502"


def test_insert_check_violation(storage, session):
    assert (
        sqlstate(storage, session, "INSERT INTO t (id, name, age) VALUES (1, 'a', -1)") == "23514"
    )


def test_insert_valid(storage, session):
    run(storage, session, "INSERT INTO t (id, name, age) VALUES (1, 'a', 5)")
    assert run(storage, session, "SELECT count(*) FROM t").rows == [(1,)]


def test_check_null_passes(storage, session):
    # age IS NULL → (age >= 0) is unknown → CHECK is satisfied.
    run(storage, session, "INSERT INTO t (id, name, age) VALUES (1, 'a', NULL)")
    assert run(storage, session, "SELECT age FROM t WHERE id = 1").rows == [(None,)]


def test_update_check_violation_leaves_table_unchanged(storage, session):
    run(storage, session, "INSERT INTO t (id, name, age) VALUES (1, 'a', 5)")
    assert sqlstate(storage, session, "UPDATE t SET age = -3 WHERE id = 1") == "23514"
    assert run(storage, session, "SELECT age FROM t WHERE id = 1").rows == [(5,)]


def test_update_not_null_violation(storage, session):
    run(storage, session, "INSERT INTO t (id, name, age) VALUES (1, 'a', 5)")
    assert sqlstate(storage, session, "UPDATE t SET name = NULL WHERE id = 1") == "23502"
    assert run(storage, session, "SELECT name FROM t WHERE id = 1").rows == [("a",)]


def test_update_valid(storage, session):
    run(storage, session, "INSERT INTO t (id, name, age) VALUES (1, 'a', 5)")
    run(storage, session, "UPDATE t SET age = 10 WHERE id = 1")
    assert run(storage, session, "SELECT age FROM t WHERE id = 1").rows == [(10,)]


def test_multi_row_insert_all_or_nothing(storage, session):
    # The second row violates CHECK; the whole statement is rejected.
    assert (
        sqlstate(
            storage,
            session,
            "INSERT INTO t (id, name, age) VALUES (1, 'a', 5), (2, 'b', -1)",
        )
        == "23514"
    )
    assert run(storage, session, "SELECT count(*) FROM t").rows == [(0,)]


def test_added_check_is_enforced(storage, session):
    # A CHECK added via ALTER TABLE is enforced on subsequent writes.
    run(storage, session, "CREATE TABLE p (id bigint primary key, score int)")
    run(storage, session, "ALTER TABLE p ADD CONSTRAINT ck_score CHECK (score > 0)")
    assert sqlstate(storage, session, "INSERT INTO p (id, score) VALUES (1, 0)") == "23514"
    run(storage, session, "INSERT INTO p (id, score) VALUES (1, 5)")
    assert run(storage, session, "SELECT score FROM p").rows == [(5,)]


def test_insert_select_enforces_check(storage, session):
    run(storage, session, "CREATE TABLE src (id bigint primary key, v int)")
    run(storage, session, "INSERT INTO src (id, v) VALUES (1, 5), (2, -2)")
    assert (
        sqlstate(storage, session, "INSERT INTO t (id, name, age) SELECT id, 'x', v FROM src")
        == "23514"
    )


def test_reflected_table_not_enforced(storage, session):
    # A schema-on-read (un-declared) collection carries no constraints; writes to
    # it aren't gated.
    storage.insert(DB, "raw", [{"_id": 1, "n": 10}])
    run(storage, session, "UPDATE raw SET n = NULL WHERE _id = 1")
    assert run(storage, session, "SELECT n FROM raw WHERE _id = 1").rows == [(None,)]


def test_on_conflict_do_update_enforces_check(storage, session):
    run(storage, session, "INSERT INTO t (id, name, age) VALUES (1, 'a', 5)")
    assert (
        sqlstate(
            storage,
            session,
            "INSERT INTO t (id, name, age) VALUES (1, 'a', 5) "
            "ON CONFLICT (id) DO UPDATE SET age = -9",
        )
        == "23514"
    )
    assert run(storage, session, "SELECT age FROM t WHERE id = 1").rows == [(5,)]
