"""P2 tests: session functions, SHOW/SET, and catalog virtual tables.

Driven through ``run_sql`` with an explicit ``Session`` (the embedded view);
the wire-level coverage lives in ``test_pgserver.py``.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from sqlfake import FakeStorage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB, user="joe", backend_pid=4242)


@pytest.fixture
def storage():
    return FakeStorage()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


# -- session / info functions ------------------------------------------------ #


def test_version(storage, session):
    res = q(storage, session, "SELECT version()")
    assert res.columns[0].name == "version"
    assert res.rows[0][0].startswith("PostgreSQL 15.0 (SecantusDB)")


def test_schema_qualified_function(storage, session):
    # SQLAlchemy's init calls pg_catalog.version() — the catalog qualifier is
    # stripped and the function evaluated.
    res = q(storage, session, "SELECT pg_catalog.version()")
    assert res.rows[0][0].startswith("PostgreSQL 15.0 (SecantusDB)")


def test_current_database_user_schema(storage, session):
    assert q(storage, session, "SELECT current_database()").rows == [("testdb",)]
    assert q(storage, session, "SELECT current_user").rows == [("joe",)]
    assert q(storage, session, "SELECT current_schema()").rows == [("public",)]


def test_pg_backend_pid_is_int4(storage, session):
    res = q(storage, session, "SELECT pg_backend_pid()")
    assert res.rows == [(4242,)]
    assert res.columns[0].type_tag == "int4"


def test_select_alias_names_column(storage, session):
    res = q(storage, session, "SELECT current_database() AS db")
    assert res.columns[0].name == "db"


# -- SET / SHOW / RESET ------------------------------------------------------ #


def test_set_show_reset_roundtrip(storage, session):
    assert q(storage, session, "SET search_path TO myschema").command_tag == "SET"
    assert q(storage, session, "SHOW search_path").rows == [("myschema",)]
    assert q(storage, session, "SELECT current_setting('search_path')").rows == [("myschema",)]
    assert q(storage, session, "RESET search_path").command_tag == "RESET"
    # Back to the default after RESET.
    assert q(storage, session, "SHOW search_path").rows == [('"$user", public',)]


def test_set_reportable_guc_surfaces_parameter_status(storage, session):
    res = q(storage, session, "SET client_encoding = 'LATIN1'")
    assert ("client_encoding", "LATIN1") in res.parameter_status


def test_transaction_control_is_accepted(storage, session):
    assert q(storage, session, "BEGIN").command_tag == "BEGIN"
    assert q(storage, session, "COMMIT").command_tag == "COMMIT"
    assert q(storage, session, "ROLLBACK").command_tag == "ROLLBACK"


# -- catalog virtual tables -------------------------------------------------- #


def _seed(storage, session):
    q(storage, session, "CREATE TABLE users (id bigint primary key, name text, age int not null)")
    q(storage, session, "CREATE TABLE orders (id bigint primary key, total numeric)")


def test_information_schema_tables(storage, session):
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT table_name, table_type FROM information_schema.tables "
        "WHERE table_schema = 'public' ORDER BY table_name",
    )
    assert res.rows == [("orders", "BASE TABLE"), ("users", "BASE TABLE")]


def test_information_schema_columns(storage, session):
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT column_name, data_type, is_nullable FROM information_schema.columns "
        "WHERE table_name = 'users' ORDER BY ordinal_position",
    )
    assert res.rows == [
        ("id", "bigint", "NO"),
        ("name", "text", "YES"),
        ("age", "integer", "NO"),
    ]


def test_pg_class_and_namespace(storage, session):
    _seed(storage, session)
    assert q(storage, session, "SELECT relname FROM pg_catalog.pg_class ORDER BY relname").rows == [
        ("orders",),
        ("users",),
    ]
    names = {r[0] for r in q(storage, session, "SELECT nspname FROM pg_catalog.pg_namespace").rows}
    assert {"pg_catalog", "public", "information_schema"} <= names


def test_pg_type_lists_known_oids(storage, session):
    res = q(storage, session, "SELECT typname FROM pg_catalog.pg_type WHERE typname = 'int8'")
    assert res.rows == [("int8",)]


def test_count_star_over_virtual_table(storage, session):
    _seed(storage, session)
    assert q(storage, session, "SELECT COUNT(*) FROM information_schema.tables").rows == [(2,)]


def test_catalog_join_class_namespace(storage, session):
    # The join interactive psql's \d emits: pg_class ⋈ pg_namespace on the
    # namespace oid. Every user table lives in ``public`` (relnamespace 2200).
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT c.relname, n.nspname FROM pg_catalog.pg_class c "
        "JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace "
        "ORDER BY c.relname",
    )
    assert res.rows == [("orders", "public"), ("users", "public")]


def test_catalog_join_with_where_on_namespace(storage, session):
    # Filtering by the joined namespace name restricts to public's relations.
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT c.relname FROM pg_catalog.pg_class c "
        "JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace "
        "WHERE n.nspname = 'public' ORDER BY c.relname",
    )
    assert res.rows == [("orders",), ("users",)]


def test_group_by_over_virtual_table(storage, session):
    # GROUP BY over a virtual catalog table goes through the aggregation pipeline
    # backed by CatalogBackend — count columns for a given base table.
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT c.table_name, COUNT(*) AS n "
        "FROM information_schema.columns c "
        "WHERE c.table_name = 'users' GROUP BY c.table_name",
    )
    assert res.rows == [("users", 3)]
