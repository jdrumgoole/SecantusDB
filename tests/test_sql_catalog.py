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
    assert q(
        storage,
        session,
        "SELECT relname FROM pg_catalog.pg_class WHERE relkind = 'r' ORDER BY relname",
    ).rows == [
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
        "WHERE c.relkind = 'r' ORDER BY c.relname",
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
        "WHERE n.nspname = 'public' AND c.relkind = 'r' ORDER BY c.relname",
    )
    assert res.rows == [("orders",), ("users",)]


def test_pg_attribute_lists_columns(storage, session):
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT attname, atttypid, attnotnull FROM pg_catalog.pg_attribute a "
        "JOIN pg_catalog.pg_class c ON a.attrelid = c.oid "
        "WHERE c.relname = 'users' ORDER BY a.attnum",
    )
    # id bigint PK (oid 20, NOT NULL), name text (25, nullable), age int (23, NOT NULL).
    assert res.rows == [("id", 20, True), ("name", 25, False), ("age", 23, True)]


def test_pg_attribute_three_way_join(storage, session):
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT a.attname FROM pg_catalog.pg_attribute a "
        "JOIN pg_catalog.pg_class c ON a.attrelid = c.oid "
        "JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace "
        "WHERE n.nspname = 'public' AND c.relname = 'orders' ORDER BY a.attnum",
    )
    assert res.rows == [("id",), ("total",)]


def test_pg_index_and_constraint_populated(storage, session):
    # A declared table with a PK has an implicit PK index relation + a 'p'
    # constraint; a user CREATE INDEX adds another (non-primary) index.
    _seed(storage, session)
    q(storage, session, "CREATE INDEX ix_age ON users (age)")
    idx = q(
        storage,
        session,
        "SELECT i.indisprimary, i.indisunique FROM pg_catalog.pg_index i "
        "JOIN pg_catalog.pg_class c ON i.indexrelid = c.oid "
        "JOIN pg_catalog.pg_class t ON i.indrelid = t.oid "
        "WHERE t.relname = 'users' ORDER BY i.indisprimary DESC",
    )
    assert (True, True) in idx.rows  # the PK index (primary + unique)
    assert (False, False) in idx.rows  # the user index on age
    # The PK surfaces as a contype 'p' constraint.
    pk = q(
        storage,
        session,
        "SELECT con.conname FROM pg_catalog.pg_constraint con "
        "JOIN pg_catalog.pg_class t ON con.conrelid = t.oid "
        "WHERE t.relname = 'users' AND con.contype = 'p'",
    )
    assert pk.rows == [("users_pkey",)]


def test_pg_attrdef_and_description_empty(storage, session):
    _seed(storage, session)
    assert q(storage, session, "SELECT * FROM pg_catalog.pg_attrdef").rows == []
    assert q(storage, session, "SELECT * FROM pg_catalog.pg_description").rows == []


def test_format_type_in_join_projection(storage, session):
    # A scalar catalog function (format_type) in the SELECT list of a join —
    # evaluated per row in Python; maps the type OID to its SQL spelling.
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT a.attname, format_type(a.atttypid, a.atttypmod) AS t "
        "FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON a.attrelid = c.oid "
        "WHERE c.relname = 'users' ORDER BY a.attnum",
    )
    assert res.rows == [("id", "bigint"), ("name", "text"), ("age", "integer")]


def test_compound_on_multikey_join(storage, session):
    # pg_attribute ⋈ pg_description on TWO equality keys (objoid=attrelid AND
    # objsubid=attnum). pg_description is empty, so a LEFT JOIN yields NULL.
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT a.attname, d.description FROM pg_catalog.pg_attribute a "
        "JOIN pg_catalog.pg_class c ON a.attrelid = c.oid "
        "LEFT OUTER JOIN pg_catalog.pg_description d "
        "ON d.objoid = a.attrelid AND d.objsubid = a.attnum "
        "WHERE c.relname = 'users' ORDER BY a.attnum",
    )
    assert res.rows == [("id", None), ("name", None), ("age", None)]


def test_residual_on_predicate(storage, session):
    # A compound ON with a residual filter on the joined table (attnum > 0) —
    # folded into the $lookup sub-pipeline.
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT a.attname FROM pg_catalog.pg_class c "
        "LEFT OUTER JOIN pg_catalog.pg_attribute a "
        "ON c.oid = a.attrelid AND a.attnum > 0 AND NOT a.attisdropped "
        "WHERE c.relname = 'users' ORDER BY a.attnum",
    )
    assert res.rows == [("id",), ("name",), ("age",)]


def test_case_and_correlated_subquery_in_projection(storage, session):
    # CASE + a correlated scalar subquery (over the empty pg_attrdef) — both
    # evaluated per row; default has no rows so the subquery is NULL.
    _seed(storage, session)
    res = q(
        storage,
        session,
        "SELECT a.attname, "
        "(SELECT d.adbin FROM pg_catalog.pg_attrdef d "
        " WHERE d.adrelid = a.attrelid AND d.adnum = a.attnum) AS deflt, "
        "CASE WHEN a.attnotnull THEN 'NN' ELSE 'null' END AS nn "
        "FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON a.attrelid = c.oid "
        "WHERE c.relname = 'users' ORDER BY a.attnum",
    )
    assert res.rows == [("id", None, "NN"), ("name", None, "null"), ("age", None, "NN")]


def test_residual_on_with_text_bound_int(storage, session):
    # Regression: a residual ON predicate comparing a numeric column to a value
    # that arrives as text (extended-protocol bind) must compare numerically, via
    # the CAST's target type — not as a string (Mongo orders numbers < strings),
    # else the join would silently drop every row.
    from secantus.sql import planner
    from secantus.sql.engine import run_statement

    _seed(storage, session)
    stmt = planner.parse(
        "SELECT a.attname FROM pg_catalog.pg_class c "
        "LEFT OUTER JOIN pg_catalog.pg_attribute a "
        "ON c.oid = a.attrelid AND a.attnum > CAST($1 AS SMALLINT) AND NOT a.attisdropped "
        "WHERE c.relname = 'users' ORDER BY a.attnum"
    )[0]
    bound = planner.substitute_parameters(stmt, ["0"])  # text-bound, as the wire does
    out = run_statement(storage, DB, bound, session)
    assert out.rows == [("id",), ("name",), ("age",)]


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
