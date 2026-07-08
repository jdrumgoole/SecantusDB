"""Constraint / sequence introspection views in ``information_schema``.

ORM and migration tooling (SQLAlchemy's inspector, Alembic autogenerate) read
``table_constraints`` / ``key_column_usage`` / ``constraint_column_usage`` /
``referential_constraints`` / ``sequences`` to reflect a schema. SecantusDB's
only real constraint is the PRIMARY KEY, so these surface PK rows (and the
FK/sequence views are present-but-empty).
"""

from __future__ import annotations

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
    sess = Session(database=DB)
    run_sql(s, DB, "CREATE TABLE users (id bigint primary key, name text)", session=sess)
    run_sql(s, DB, "CREATE TABLE orders (id bigint primary key, total int)", session=sess)
    try:
        yield s
    finally:
        s.close()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


def test_table_constraints_lists_primary_keys(storage, session):
    res = q(
        storage,
        session,
        "SELECT table_name, constraint_name, constraint_type "
        "FROM information_schema.table_constraints ORDER BY table_name",
    )
    assert res.rows == [
        ("orders", "orders_pkey", "PRIMARY KEY"),
        ("users", "users_pkey", "PRIMARY KEY"),
    ]


def test_key_column_usage(storage, session):
    res = q(
        storage,
        session,
        "SELECT constraint_name, column_name, ordinal_position "
        "FROM information_schema.key_column_usage "
        "WHERE table_name = 'users'",
    )
    assert res.rows == [("users_pkey", "id", 1)]


def test_constraint_column_usage(storage, session):
    res = q(
        storage,
        session,
        "SELECT table_name, column_name, constraint_name "
        "FROM information_schema.constraint_column_usage ORDER BY table_name",
    )
    assert res.rows == [
        ("orders", "id", "orders_pkey"),
        ("users", "id", "users_pkey"),
    ]


def test_referential_constraints_empty(storage, session):
    # No FKs in the model — the view resolves but is empty.
    res = q(
        storage,
        session,
        "SELECT count(*) FROM information_schema.referential_constraints",
    )
    assert res.rows == [(0,)]


def test_sequences_empty(storage, session):
    res = q(storage, session, "SELECT count(*) FROM information_schema.sequences")
    assert res.rows == [(0,)]


def test_join_constraints_to_key_columns(storage, session):
    # The canonical ORM reflection join: table_constraints ⋈ key_column_usage.
    res = q(
        storage,
        session,
        "SELECT tc.table_name, kcu.column_name "
        "FROM information_schema.table_constraints tc "
        "JOIN information_schema.key_column_usage kcu "
        "ON tc.constraint_name = kcu.constraint_name "
        "WHERE tc.constraint_type = 'PRIMARY KEY' ORDER BY tc.table_name",
    )
    assert res.rows == [("orders", "id"), ("users", "id")]
