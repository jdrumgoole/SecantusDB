"""Enforcement of UNIQUE constraints on write (``23505``).

An INSERT or UPDATE that would create two rows sharing a value for a declared
UNIQUE constraint is rejected and the table is left unchanged. NULLs are
*distinct* (multiple NULLs are allowed), matching Postgres' default. The PK is
enforced separately by storage (the ``_id`` index); this covers the declared
non-PK UNIQUE constraints from ``CREATE TABLE`` / ``ALTER TABLE``.
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
        "CREATE TABLE t (id bigint primary key, email text UNIQUE, a int, b int, "
        "CONSTRAINT uq_ab UNIQUE (a, b))",
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


def test_insert_duplicate_single_column(storage, session):
    run(storage, session, "INSERT INTO t (id, email) VALUES (1, 'x')")
    assert sqlstate(storage, session, "INSERT INTO t (id, email) VALUES (2, 'x')") == "23505"
    assert rows(storage, session, "SELECT count(*) FROM t") == [(1,)]


def test_insert_duplicate_compound(storage, session):
    run(storage, session, "INSERT INTO t (id, a, b) VALUES (1, 1, 2)")
    assert sqlstate(storage, session, "INSERT INTO t (id, a, b) VALUES (2, 1, 2)") == "23505"


def test_compound_partial_overlap_allowed(storage, session):
    # (a, b) unique — same a, different b is fine.
    run(storage, session, "INSERT INTO t (id, a, b) VALUES (1, 1, 2)")
    run(storage, session, "INSERT INTO t (id, a, b) VALUES (2, 1, 3)")
    assert rows(storage, session, "SELECT count(*) FROM t") == [(2,)]


def test_nulls_are_distinct(storage, session):
    run(storage, session, "INSERT INTO t (id, email) VALUES (1, NULL)")
    run(storage, session, "INSERT INTO t (id, email) VALUES (2, NULL)")
    assert rows(storage, session, "SELECT count(*) FROM t") == [(2,)]


def test_compound_null_exempt(storage, session):
    # A NULL in any column of a compound UNIQUE exempts the row.
    run(storage, session, "INSERT INTO t (id, a, b) VALUES (1, 1, NULL)")
    run(storage, session, "INSERT INTO t (id, a, b) VALUES (2, 1, NULL)")
    assert rows(storage, session, "SELECT count(*) FROM t") == [(2,)]


def test_within_batch_duplicate_rejected(storage, session):
    assert (
        sqlstate(storage, session, "INSERT INTO t (id, email) VALUES (1, 'p'), (2, 'p')") == "23505"
    )
    assert rows(storage, session, "SELECT count(*) FROM t") == [(0,)]


def test_update_creating_duplicate(storage, session):
    run(storage, session, "INSERT INTO t (id, email) VALUES (1, 'a'), (2, 'b')")
    assert sqlstate(storage, session, "UPDATE t SET email = 'a' WHERE id = 2") == "23505"
    assert rows(storage, session, "SELECT email FROM t WHERE id = 2") == [("b",)]


def test_update_self_same_value_allowed(storage, session):
    run(storage, session, "INSERT INTO t (id, email) VALUES (1, 'a')")
    run(storage, session, "UPDATE t SET email = 'a' WHERE id = 1")
    assert rows(storage, session, "SELECT email FROM t WHERE id = 1") == [("a",)]


def test_update_multi_row_to_same_value_rejected(storage, session):
    run(storage, session, "INSERT INTO t (id, email) VALUES (1, 'a'), (2, 'b')")
    assert sqlstate(storage, session, "UPDATE t SET email = 'z' WHERE id IN (1, 2)") == "23505"
    assert sorted(rows(storage, session, "SELECT id, email FROM t")) == [(1, "a"), (2, "b")]


def test_update_distinct_values_allowed(storage, session):
    run(storage, session, "INSERT INTO t (id, email) VALUES (1, 'a'), (2, 'b')")
    run(storage, session, "UPDATE t SET email = 'c' WHERE id = 2")
    assert sorted(rows(storage, session, "SELECT id, email FROM t")) == [(1, "a"), (2, "c")]


def test_altered_unique_is_enforced(storage, session):
    run(storage, session, "CREATE TABLE p (id bigint primary key, code text)")
    run(storage, session, "ALTER TABLE p ADD CONSTRAINT uq_code UNIQUE (code)")
    run(storage, session, "INSERT INTO p (id, code) VALUES (1, 'k')")
    assert sqlstate(storage, session, "INSERT INTO p (id, code) VALUES (2, 'k')") == "23505"


def test_insert_select_enforces_unique(storage, session):
    run(storage, session, "CREATE TABLE src (id bigint primary key, e text)")
    run(storage, session, "INSERT INTO src (id, e) VALUES (1, 'dup'), (2, 'dup')")
    assert sqlstate(storage, session, "INSERT INTO t (id, email) SELECT id, e FROM src") == "23505"


def test_reflected_table_not_enforced(storage, session):
    # Un-declared collections have no UNIQUE constraints.
    storage.insert(DB, "raw", [{"_id": 1, "k": "v"}])
    run(storage, session, "INSERT INTO raw (_id, k) VALUES (2, 'v')")
    assert rows(storage, session, "SELECT count(*) FROM raw") == [(2,)]


def test_error_names_the_constraint(storage, session):
    run(storage, session, "INSERT INTO t (id, email) VALUES (1, 'x')")
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, "INSERT INTO t (id, email) VALUES (2, 'x')")
    assert "t_email_key" in str(ei.value)
