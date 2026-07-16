"""Declared foreign keys — stored in the catalog, surfaced via reflection.

FKs are **declared, never enforced** (SecantusDB does no referential-integrity
check on write). What matters is that they reflect: `information_schema`'s
constraint views, `pg_catalog.pg_constraint` (contype 'f'), and the
`pg_get_constraintdef` string SQLAlchemy's inspector regex-parses all see them.
"""

from __future__ import annotations

import pytest
import sqlglot

from secantus.sql import planner, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(session, tmp_path):
    s = Storage(str(tmp_path))
    run_sql(s, DB, "CREATE TABLE users (id bigint primary key, name text)", session=session)
    run_sql(
        s,
        DB,
        "CREATE TABLE orders (id bigint primary key, "
        "user_id bigint REFERENCES users(id) ON DELETE CASCADE, total int)",
        session=session,
    )
    try:
        yield s
    finally:
        s.close()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


# -- parsing (planner) ------------------------------------------------------ #


def _fks(sql):
    return planner.plan_create_table(sqlglot.parse_one(sql, read="postgres")).table.foreign_keys


def test_column_level_reference_parsed():
    (fk,) = _fks("CREATE TABLE o (id bigint primary key, uid bigint REFERENCES users(id))")
    assert fk.name == "o_uid_fkey"
    assert fk.columns == ("uid",)
    assert fk.ref_table == "users"
    assert fk.ref_columns == ("id",)
    assert fk.on_delete is None and fk.on_update is None


def test_table_level_foreign_key_parsed():
    (fk,) = _fks(
        "CREATE TABLE o (id bigint primary key, uid bigint, "
        "FOREIGN KEY (uid) REFERENCES users(id) ON DELETE CASCADE ON UPDATE SET NULL)"
    )
    assert fk.columns == ("uid",)
    assert fk.on_delete == "CASCADE"
    assert fk.on_update == "SET NULL"


def test_reference_without_columns_targets_pk():
    (fk,) = _fks("CREATE TABLE o (id bigint primary key, uid bigint REFERENCES users)")
    assert fk.ref_table == "users"
    assert fk.ref_columns == ()  # resolved to the target's PK at reflection time


def test_no_foreign_keys():
    assert _fks("CREATE TABLE o (id bigint primary key, n int)") == []


def test_column_level_named_constraint_keeps_name():
    (fk,) = _fks(
        "CREATE TABLE o (id bigint primary key, "
        "uid bigint CONSTRAINT o_uid_ref REFERENCES users(id) ON DELETE CASCADE)"
    )
    assert fk.name == "o_uid_ref"  # explicit CONSTRAINT name, not the auto o_uid_fkey
    assert fk.columns == ("uid",)
    assert fk.ref_table == "users"
    assert fk.on_delete == "CASCADE"


def test_table_level_named_foreign_key_parsed():
    (fk,) = _fks(
        "CREATE TABLE o (id bigint primary key, uid bigint, "
        "CONSTRAINT o_uid_ref FOREIGN KEY (uid) REFERENCES users(id) "
        "ON DELETE SET NULL DEFERRABLE INITIALLY DEFERRED)"
    )
    assert fk.name == "o_uid_ref"
    assert fk.columns == ("uid",)
    assert fk.ref_table == "users"
    assert fk.ref_columns == ("id",)
    assert fk.on_delete == "SET NULL"
    assert fk.deferrable is True
    assert fk.initially_deferred is True


def test_table_level_named_composite_foreign_key_parsed():
    (fk,) = _fks(
        "CREATE TABLE o (a bigint, b bigint, "
        "CONSTRAINT o_ab_fk FOREIGN KEY (a, b) REFERENCES users(x, y))"
    )
    assert fk.name == "o_ab_fk"
    assert fk.columns == ("a", "b")
    assert fk.ref_columns == ("x", "y")


# -- catalog round-trip ----------------------------------------------------- #


def test_foreign_key_persists_in_catalog(storage, session):
    from secantus.sql.catalog import Catalog

    tbl = Catalog(storage).get(DB, "orders")
    assert tbl is not None
    (fk,) = tbl.foreign_keys
    assert fk.name == "orders_user_id_fkey"
    assert fk.ref_table == "users"
    assert fk.on_delete == "CASCADE"


def test_table_level_named_fk_enforces_and_reflects(session, tmp_path):
    """A table-level ``CONSTRAINT n FOREIGN KEY`` enforces on write under its
    explicit name and reflects through ``pg_constraint``."""
    s = Storage(str(tmp_path))
    run_sql(s, DB, "CREATE TABLE users (id bigint primary key, name text)", session=session)
    run_sql(s, DB, "INSERT INTO users (id, name) VALUES (1, 'a')", session=session)
    run_sql(
        s,
        DB,
        "CREATE TABLE orders (id bigint primary key, uid bigint, "
        "CONSTRAINT orders_uid_ref FOREIGN KEY (uid) REFERENCES users(id))",
        session=session,
    )
    # Enforced: a child pointing at a missing parent is rejected (23503).
    from secantus.sql import errors

    with pytest.raises(errors.SQLError) as ei:
        run_sql(s, DB, "INSERT INTO orders (id, uid) VALUES (10, 99)", session=session)
    assert ei.value.sqlstate == "23503"
    run_sql(s, DB, "INSERT INTO orders (id, uid) VALUES (10, 1)", session=session)  # OK
    # Reflected under the explicit name.
    rows = q(
        s,
        session,
        "SELECT conname FROM pg_catalog.pg_constraint WHERE contype = 'f'",
    ).rows
    assert ("orders_uid_ref",) in rows


# -- information_schema reflection ------------------------------------------ #


def test_referential_constraints(storage, session):
    rows = q(
        storage,
        session,
        "SELECT constraint_name, unique_constraint_name, update_rule, delete_rule "
        "FROM information_schema.referential_constraints",
    ).rows
    assert rows == [("orders_user_id_fkey", "users_pkey", "NO ACTION", "CASCADE")]


def test_table_constraints_lists_foreign_key(storage, session):
    rows = q(
        storage,
        session,
        "SELECT constraint_name, constraint_type FROM information_schema.table_constraints "
        "WHERE table_name = 'orders' ORDER BY constraint_type",
    ).rows
    assert ("orders_user_id_fkey", "FOREIGN KEY") in rows
    assert ("orders_pkey", "PRIMARY KEY") in rows


def test_key_column_usage_fk_columns(storage, session):
    rows = q(
        storage,
        session,
        "SELECT column_name, position_in_unique_constraint "
        "FROM information_schema.key_column_usage "
        "WHERE constraint_name = 'orders_user_id_fkey'",
    ).rows
    assert rows == [("user_id", 1)]


def test_constraint_column_usage_names_referenced_columns(storage, session):
    rows = q(
        storage,
        session,
        "SELECT table_name, column_name FROM information_schema.constraint_column_usage "
        "WHERE constraint_name = 'orders_user_id_fkey'",
    ).rows
    assert rows == [("users", "id")]


# -- pg_catalog reflection -------------------------------------------------- #


def test_pg_constraint_foreign_key_row(storage, session):
    rows = q(
        storage,
        session,
        "SELECT conname, contype FROM pg_catalog.pg_constraint WHERE contype = 'f'",
    ).rows
    assert rows == [("orders_user_id_fkey", "f")]


def test_pg_get_constraintdef_renders_fk(storage, session):
    # The string SQLAlchemy's inspector regex-parses (scalar.py calls this helper
    # when it evaluates pg_get_constraintdef(oid) inside the reflection join).
    from secantus.sql import virtual
    from secantus.sql.catalog import Catalog

    catalog = Catalog(storage)
    oid = q(
        storage,
        session,
        "SELECT oid FROM pg_catalog.pg_constraint WHERE conname = 'orders_user_id_fkey'",
    ).rows[0][0]
    assert (
        virtual.constraint_def_for_oid(DB, catalog, oid)
        == "FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE"
    )
    assert virtual.constraint_def_for_oid(DB, catalog, 999999) is None
