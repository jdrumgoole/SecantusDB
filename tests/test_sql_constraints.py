"""CHECK / UNIQUE constraints — declared at ``CREATE TABLE``, recorded in the
catalog, and reflected through ``pg_constraint`` (contype 'c'/'u'),
``information_schema``, and ``pg_get_constraintdef`` so SQLAlchemy's
``get_check_constraints`` / ``get_unique_constraints`` see them.

Neither is enforced — SecantusDB does not validate CHECK predicates or reject
duplicate UNIQUE values on write. This is a schema-shape record for reflection.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"

DDL = """CREATE TABLE t (
  id bigint primary key,
  email text UNIQUE,
  age int CHECK (age >= 0),
  status text,
  CONSTRAINT uq_es UNIQUE (email, status),
  CONSTRAINT ck_age CHECK (age < 200),
  UNIQUE (status)
)"""


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(session, tmp_path):
    s = Storage(str(tmp_path))
    run_sql(s, DB, DDL, session=session)
    try:
        yield s
    finally:
        s.close()


def rows(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1].rows


def test_constraints_persist_in_catalog(storage, session):
    from secantus.sql.catalog import Catalog

    t = Catalog(storage).get(DB, "t")
    assert t is not None
    checks = {c.name: c.expression for c in t.check_constraints}
    assert checks == {"t_age_check": "age >= 0", "ck_age": "age < 200"}
    uniques = {u.name: u.columns for u in t.unique_constraints}
    assert uniques == {
        "t_email_key": ("email",),
        "uq_es": ("email", "status"),
        "t_status_key": ("status",),
    }


def test_pg_constraint_contypes(storage, session):
    assert rows(
        storage,
        session,
        "SELECT conname, contype FROM pg_catalog.pg_constraint "
        "WHERE contype IN ('u', 'c') ORDER BY contype, conname",
    ) == [
        ("ck_age", "c"),
        ("t_age_check", "c"),
        ("t_email_key", "u"),
        ("t_status_key", "u"),
        ("uq_es", "u"),
    ]


def test_pg_get_constraintdef_unique(storage, session):
    assert rows(
        storage,
        session,
        "SELECT pg_get_constraintdef(oid) FROM pg_catalog.pg_constraint WHERE conname = 'uq_es'",
    ) == [("UNIQUE (email, status)",)]


def test_pg_get_constraintdef_check(storage, session):
    assert rows(
        storage,
        session,
        "SELECT pg_get_constraintdef(oid) FROM pg_catalog.pg_constraint WHERE conname = 'ck_age'",
    ) == [("CHECK ((age < 200))",)]


def test_information_schema_check_constraints(storage, session):
    assert rows(
        storage,
        session,
        "SELECT constraint_name, check_clause FROM information_schema.check_constraints "
        "ORDER BY constraint_name",
    ) == [("ck_age", "(age < 200)"), ("t_age_check", "(age >= 0)")]


def test_information_schema_table_constraints(storage, session):
    assert rows(
        storage,
        session,
        "SELECT constraint_name, constraint_type FROM information_schema.table_constraints "
        "WHERE constraint_type IN ('UNIQUE', 'CHECK') ORDER BY constraint_type, constraint_name",
    ) == [
        ("ck_age", "CHECK"),
        ("t_age_check", "CHECK"),
        ("t_email_key", "UNIQUE"),
        ("t_status_key", "UNIQUE"),
        ("uq_es", "UNIQUE"),
    ]


def test_unique_backing_index_in_pg_index(storage, session):
    # A UNIQUE constraint is backed by a unique index whose indexrelid == conindid.
    assert rows(
        storage,
        session,
        "SELECT i.indisunique, i.indisprimary FROM pg_catalog.pg_index i "
        "JOIN pg_catalog.pg_constraint c ON c.conindid = i.indexrelid "
        "WHERE c.conname = 'uq_es'",
    ) == [(True, False)]


def test_key_column_usage_lists_unique_columns(storage, session):
    assert rows(
        storage,
        session,
        "SELECT column_name, ordinal_position FROM information_schema.key_column_usage "
        "WHERE constraint_name = 'uq_es' ORDER BY ordinal_position",
    ) == [("email", 1), ("status", 2)]


def test_no_constraints_is_empty(storage, session):
    # A table with only a PK has no UNIQUE/CHECK rows (just its contype 'p').
    run_sql(storage, DB, "CREATE TABLE plain (id bigint primary key, n int)", session=session)
    assert rows(
        storage,
        session,
        "SELECT c.contype FROM pg_catalog.pg_constraint c "
        "JOIN pg_catalog.pg_class r ON r.oid = c.conrelid "
        "WHERE r.relname = 'plain'",
    ) == [("p",)]


def test_sqlalchemy_reflects_check_and_unique(storage, session, tmp_path):
    sa = pytest.importorskip("sqlalchemy")
    from secantus.sql.pgserver import SecantusPGServer

    st = Storage(str(tmp_path / "pg"))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        host, port = srv.address
        engine = sa.create_engine(f"postgresql+pg8000://joe@{host}:{port}/db")
        with engine.begin() as conn:
            conn.execute(sa.text(DDL))
        insp = sa.inspect(engine)

        uniques = {u["name"]: tuple(u["column_names"]) for u in insp.get_unique_constraints("t")}
        assert uniques == {
            "t_email_key": ("email",),
            "uq_es": ("email", "status"),
            "t_status_key": ("status",),
        }

        checks = {c["name"]: c["sqltext"] for c in insp.get_check_constraints("t")}
        assert checks == {"t_age_check": "age >= 0", "ck_age": "age < 200"}
        engine.dispose()
    finally:
        srv.stop()
        st.close()
