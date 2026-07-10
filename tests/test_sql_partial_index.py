"""Partial indexes (``CREATE INDEX … WHERE …``).

A partial-index predicate lowers to a Mongo filter passed to storage as
``partialFilterExpression`` — the same option the MongoDB-side partial index uses,
so the query planner accelerates matching queries. (Expression indexes —
``CREATE INDEX … ((a + b))`` — are now supported too; they have their own suite in
``test_sql_expr_index.py``.)
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
def storage(session, tmp_path):
    s = Storage(str(tmp_path))
    run(s, session, "CREATE TABLE t (id int PRIMARY KEY, a int, b int, status text)")
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def _index(storage, name):
    return next((i for i in storage.list_indexes(DB, "t") if i["name"] == name), None)


def test_partial_index_carries_filter(storage, session):
    run(storage, session, "CREATE INDEX ix ON t (a) WHERE a > 0")
    ix = _index(storage, "ix")
    assert ix["key"] == {"a": 1}
    assert ix["partialFilterExpression"] == {"a": {"$gt": 0}}


def test_partial_index_string_predicate(storage, session):
    run(storage, session, "CREATE INDEX ix ON t (status) WHERE status = 'active'")
    assert _index(storage, "ix")["partialFilterExpression"] == {"status": "active"}


def test_partial_index_compound_predicate(storage, session):
    run(storage, session, "CREATE INDEX ix ON t (a) WHERE a > 0 AND status = 'active'")
    pf = _index(storage, "ix")["partialFilterExpression"]
    # AND of two field predicates merges into one filter document.
    assert pf == {"a": {"$gt": 0}, "status": "active"}


def test_plain_index_has_no_partial_filter(storage, session):
    run(storage, session, "CREATE INDEX ix ON t (a)")
    assert "partialFilterExpression" not in _index(storage, "ix")


def test_unique_partial_index(storage, session):
    run(storage, session, "CREATE UNIQUE INDEX ix ON t (a) WHERE a IS NOT NULL")
    ix = _index(storage, "ix")
    assert ix.get("unique") is True
    assert ix["partialFilterExpression"] == {"a": {"$ne": None}}


def test_partial_index_if_not_exists(storage, session):
    run(storage, session, "CREATE INDEX ix ON t (a) WHERE a > 0")
    assert (
        run(storage, session, "CREATE INDEX IF NOT EXISTS ix ON t (a) WHERE a > 0").command_tag
        == "CREATE INDEX"
    )


def test_index_still_reflects_in_pg_index(storage, session):
    run(storage, session, "CREATE INDEX ix ON t (a) WHERE a > 0")
    # The partial index is still a normal pg_class / pg_index relation.
    rows = run(
        storage,
        session,
        "SELECT relname FROM pg_catalog.pg_class WHERE relkind = 'i' AND relname = 'ix'",
    ).rows
    assert rows == [("ix",)]
