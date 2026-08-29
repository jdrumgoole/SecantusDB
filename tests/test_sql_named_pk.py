"""A table-level ``CONSTRAINT <name> PRIMARY KEY`` — previously silently
dropped (no ``_id`` mapping, no uniqueness). The declared name is honored
end-to-end: enforcement, ``pg_constraint`` reflection, duplicate-key error
messages, and ``ON CONFLICT ON CONSTRAINT``.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.catalog import Catalog
from secantus.sql.errors import SQLError
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
def named_pk_table(storage, session):
    run(
        storage,
        session,
        "CREATE TABLE t (a int, b text, CONSTRAINT my_pk PRIMARY KEY (a))",
    )
    return storage


def test_named_pk_maps_to_id(named_pk_table, session):
    table = Catalog(named_pk_table).get(DB, "t")
    assert [c.name for c in table.columns if c.field == "_id"] == ["a"]
    assert table.pk_name == "my_pk"
    assert table.pk_constraint_name() == "my_pk"


def test_named_pk_enforced(named_pk_table, session):
    run(named_pk_table, session, "INSERT INTO t VALUES (1, 'x')")
    with pytest.raises(SQLError) as exc:
        run(named_pk_table, session, "INSERT INTO t VALUES (1, 'y')")
    assert exc.value.sqlstate == "23505"


def test_named_pk_reflects_declared_name(named_pk_table, session):
    res = run(
        named_pk_table,
        session,
        "SELECT conname FROM pg_constraint WHERE contype = 'p'",
    )
    assert ("my_pk",) in res.rows


def test_named_composite_pk(storage, session):
    run(
        storage,
        session,
        "CREATE TABLE c (a int, b int, CONSTRAINT ab_pk PRIMARY KEY (a, b))",
    )
    run(storage, session, "INSERT INTO c VALUES (1, 2)")
    run(storage, session, "INSERT INTO c VALUES (1, 3)")
    with pytest.raises(SQLError) as exc:
        run(storage, session, "INSERT INTO c VALUES (1, 2)")
    assert exc.value.sqlstate == "23505"


def test_on_conflict_on_named_constraint(named_pk_table, session):
    run(named_pk_table, session, "INSERT INTO t VALUES (1, 'x')")
    run(
        named_pk_table,
        session,
        "INSERT INTO t VALUES (1, 'y') ON CONFLICT ON CONSTRAINT my_pk DO UPDATE SET b = 'y'",
    )
    res = run(named_pk_table, session, "SELECT b FROM t WHERE a = 1")
    assert res.rows == [("y",)]


def test_unnamed_table_level_pk_still_works(storage, session):
    run(storage, session, "CREATE TABLE u (a int, b text, PRIMARY KEY (a))")
    table = Catalog(storage).get(DB, "u")
    assert [c.name for c in table.columns if c.field == "_id"] == ["a"]
    assert table.pk_constraint_name() == "u_pkey"


def test_pk_comment_via_declared_name(named_pk_table, session):
    run(named_pk_table, session, "COMMENT ON CONSTRAINT my_pk ON t IS 'pk comment'")
    table = Catalog(named_pk_table).get(DB, "t")
    assert table.pk_comment == "pk comment"
