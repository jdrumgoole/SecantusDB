"""TRUNCATE TABLE (#133).

``TRUNCATE [TABLE] t [, …] [RESTART IDENTITY | CONTINUE IDENTITY]
[CASCADE | RESTRICT]`` empties each table fast. ``RESTART IDENTITY`` resets owned
sequences; ``CASCADE`` also truncates referencing tables (transitively) while the
default ``RESTRICT`` errors if a table is referenced from outside the set. Driven
over the real ``Storage``.
"""

from __future__ import annotations

import pytest

from secantus.sql import SQLError, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "app"


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def _sess():
    return Session(database=DB)


def _rows(storage, sql, session=None):
    return run_sql(storage, DB, sql, session=session or _sess())[-1].rows


def _count(storage, table):
    return _rows(storage, f"SELECT count(*) FROM {table}")[0][0]


# --------------------------------------------------------------------------- #
# Basic empty.
# --------------------------------------------------------------------------- #


def test_truncate_empties_table(storage):
    s = _sess()
    run_sql(storage, DB, "CREATE TABLE t (id bigint primary key, v int)", session=s)
    run_sql(storage, DB, "INSERT INTO t VALUES (1,10),(2,20),(3,30)", session=s)
    assert _count(storage, "t") == 3
    r = run_sql(storage, DB, "TRUNCATE t", session=s)[-1]
    assert r.command_tag == "TRUNCATE TABLE"
    assert _count(storage, "t") == 0


def test_truncate_table_keyword_and_multiple(storage):
    s = _sess()
    run_sql(storage, DB, "CREATE TABLE t (id bigint primary key)", session=s)
    run_sql(storage, DB, "CREATE TABLE u (id bigint primary key)", session=s)
    run_sql(storage, DB, "INSERT INTO t VALUES (1)", session=s)
    run_sql(storage, DB, "INSERT INTO u VALUES (1)", session=s)
    run_sql(storage, DB, "TRUNCATE TABLE t, u", session=s)
    assert _count(storage, "t") == 0 and _count(storage, "u") == 0


def test_truncate_unknown_table_errors(storage):
    with pytest.raises(SQLError) as ei:
        run_sql(storage, DB, "TRUNCATE nope", session=_sess())
    assert ei.value.sqlstate == "42P01"


def test_truncate_if_exists(storage):
    r = run_sql(storage, DB, "TRUNCATE IF EXISTS nope", session=_sess())[-1]
    assert r.command_tag == "TRUNCATE TABLE"


# --------------------------------------------------------------------------- #
# Identity reset.
# --------------------------------------------------------------------------- #


def test_continue_identity_is_default(storage):
    s = _sess()
    run_sql(storage, DB, "CREATE TABLE t (id serial primary key, v int)", session=s)
    run_sql(storage, DB, "INSERT INTO t (v) VALUES (1),(2),(3)", session=s)
    run_sql(storage, DB, "TRUNCATE t", session=s)  # CONTINUE IDENTITY
    run_sql(storage, DB, "INSERT INTO t (v) VALUES (9)", session=s)
    assert _rows(storage, "SELECT id FROM t") == [(4,)]


def test_restart_identity_resets_sequence(storage):
    s = _sess()
    run_sql(storage, DB, "CREATE TABLE t (id serial primary key, v int)", session=s)
    run_sql(storage, DB, "INSERT INTO t (v) VALUES (1),(2),(3)", session=s)
    run_sql(storage, DB, "TRUNCATE t RESTART IDENTITY", session=s)
    run_sql(storage, DB, "INSERT INTO t (v) VALUES (9)", session=s)
    assert _rows(storage, "SELECT id FROM t") == [(1,)]


# --------------------------------------------------------------------------- #
# Foreign-key RESTRICT / CASCADE.
# --------------------------------------------------------------------------- #


@pytest.fixture
def fk_storage(storage):
    s = _sess()
    run_sql(storage, DB, "CREATE TABLE parent (id bigint primary key)", session=s)
    run_sql(
        storage,
        DB,
        "CREATE TABLE child (id bigint primary key, p int REFERENCES parent(id))",
        session=s,
    )
    run_sql(storage, DB, "INSERT INTO parent VALUES (1),(2)", session=s)
    run_sql(storage, DB, "INSERT INTO child VALUES (10,1),(20,2)", session=s)
    return storage


def test_restrict_errors_on_outside_reference(fk_storage):
    with pytest.raises(SQLError) as ei:
        run_sql(fk_storage, DB, "TRUNCATE parent", session=_sess())
    assert ei.value.sqlstate == "0A000"


def test_referenced_table_truncatable_alone(fk_storage):
    # Nothing references child, so truncating it alone is fine.
    run_sql(fk_storage, DB, "TRUNCATE child", session=_sess())
    assert _count(fk_storage, "child") == 0
    assert _count(fk_storage, "parent") == 2


def test_cascade_truncates_referencing_tables(fk_storage):
    run_sql(fk_storage, DB, "TRUNCATE parent CASCADE", session=_sess())
    assert _count(fk_storage, "parent") == 0
    assert _count(fk_storage, "child") == 0


def test_truncating_both_together_is_allowed(fk_storage):
    run_sql(fk_storage, DB, "TRUNCATE parent, child", session=_sess())
    assert _count(fk_storage, "parent") == 0
    assert _count(fk_storage, "child") == 0


# --------------------------------------------------------------------------- #
# Authorization: TRUNCATE is a write (needs the remove action).
# --------------------------------------------------------------------------- #


def test_truncate_needs_write_privilege(storage):
    s = _sess()
    run_sql(storage, DB, "CREATE TABLE t (id bigint primary key)", session=s)
    run_sql(storage, DB, "INSERT INTO t VALUES (1)", session=s)
    reader = Session(database=DB, user="joe", authz_active=True, roles=[{"role": "read", "db": DB}])
    with pytest.raises(SQLError) as ei:
        run_sql(storage, DB, "TRUNCATE t", session=reader)
    assert ei.value.sqlstate == "42501"
    writer = Session(
        database=DB, user="jo", authz_active=True, roles=[{"role": "readWrite", "db": DB}]
    )
    run_sql(storage, DB, "TRUNCATE t", session=writer)
    assert _count(storage, "t") == 0
