"""``ALTER TABLE … ADD [CONSTRAINT name] FOREIGN KEY`` — declared, reflected.

Adds a foreign key to an existing table's catalog entry (never enforced, like a
CREATE TABLE FK). It reflects through the same ``information_schema`` /
``pg_catalog`` views. Non-FK constraints (CHECK / UNIQUE) are rejected.
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
def storage(session, tmp_path):
    s = Storage(str(tmp_path))

    def q(sql):
        run_sql(s, DB, sql, session=session)

    q("CREATE TABLE users (id bigint primary key, name text)")
    q("CREATE TABLE orders (id bigint primary key, user_id bigint, total int)")
    try:
        yield s
    finally:
        s.close()


def rows(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1].rows


def test_add_unnamed_foreign_key(storage, session):
    run_sql(
        storage,
        DB,
        "ALTER TABLE orders ADD FOREIGN KEY (user_id) REFERENCES users(id)",
        session=session,
    )
    assert rows(
        storage,
        session,
        "SELECT constraint_name, unique_constraint_name FROM "
        "information_schema.referential_constraints",
    ) == [("orders_user_id_fkey", "users_pkey")]


def test_add_named_constraint_with_actions(storage, session):
    run_sql(
        storage,
        DB,
        "ALTER TABLE orders ADD CONSTRAINT fk_ou FOREIGN KEY (user_id) "
        "REFERENCES users(id) ON DELETE CASCADE ON UPDATE SET NULL",
        session=session,
    )
    assert rows(
        storage,
        session,
        "SELECT constraint_name, update_rule, delete_rule FROM "
        "information_schema.referential_constraints WHERE constraint_name = 'fk_ou'",
    ) == [("fk_ou", "SET NULL", "CASCADE")]


def test_added_fk_in_pg_constraint(storage, session):
    run_sql(
        storage,
        DB,
        "ALTER TABLE orders ADD CONSTRAINT fk_ou FOREIGN KEY (user_id) REFERENCES users(id)",
        session=session,
    )
    assert rows(
        storage,
        session,
        "SELECT conname, contype FROM pg_catalog.pg_constraint WHERE contype = 'f'",
    ) == [("fk_ou", "f")]


def test_add_check_constraint_now_supported(storage, session):
    # ADD CONSTRAINT ... CHECK is now modeled (declared, reflected, not enforced).
    run_sql(
        storage,
        DB,
        "ALTER TABLE orders ADD CONSTRAINT ck CHECK (total > 0)",
        session=session,
    )
    assert run_sql(
        storage,
        DB,
        "SELECT conname, contype FROM pg_catalog.pg_constraint WHERE contype = 'c'",
        session=session,
    )[-1].rows == [("ck", "c")]


def test_added_fk_persists_in_catalog(storage, session):
    from secantus.sql.catalog import Catalog

    run_sql(
        storage,
        DB,
        "ALTER TABLE orders ADD FOREIGN KEY (user_id) REFERENCES users(id)",
        session=session,
    )
    tbl = Catalog(storage).get(DB, "orders")
    assert tbl is not None
    (fk,) = tbl.foreign_keys
    assert fk.columns == ("user_id",)
    assert fk.ref_table == "users"
