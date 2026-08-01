"""Set-returning functions as join and derived-table sources.

An SRF used to work only as the *sole* FROM item. Anywhere else — a JOIN
source, or the body of a derived table — sqlglot models it as an ``exp.Table``
whose ``this`` is the function node, so the name was empty and the planner
reported ``relation "" does not exist``. That was the single largest error
message in the pgjdbc gauge (73), emitted by its ``TypeInfoCache`` type
lookup.
"""

from __future__ import annotations

import pytest

from secantus.sql.engine import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

# The type-lookup query pgjdbc issues for every type it has not cached. It
# stacks all the pieces at once: an SRF inside a derived table, JOIN … USING
# against it, a LEFT JOIN over the result, and an SRF argument that reads
# session state (current_schemas).
TYPEINFO_SQL = (
    "SELECT typinput='pg_catalog.array_in'::regproc as is_array, typtype, typname, pg_type.oid "
    "  FROM pg_catalog.pg_type "
    "  LEFT JOIN (select ns.oid as nspoid, ns.nspname, r.r "
    "          from pg_namespace as ns "
    "          join ( select s.r, (current_schemas(false))[s.r] as nspname "
    "                   from generate_series(1, array_upper(current_schemas(false), 1)) "
    "                   as s(r) ) as r "
    "         using ( nspname ) "
    "       ) as sp "
    "    ON sp.nspoid = typnamespace "
    " WHERE pg_type.oid = 23 "
    " ORDER BY sp.r, pg_type.oid DESC"
)


@pytest.fixture()
def db(tmp_path):
    storage = Storage(str(tmp_path))
    session = Session(database="t")

    def run(sql: str):
        return [r.rows for r in run_sql(storage, "t", sql, session=session)][0]

    for ddl in (
        "CREATE TABLE a(k int, v text)",
        "INSERT INTO a VALUES (1, 'x'), (2, 'y'), (5, 'z')",
    ):
        run(ddl)
    try:
        yield run
    finally:
        storage.close()


class TestSoleFromStillWorks:
    def test_bare_srf(self, db):
        assert db("SELECT * FROM generate_series(1, 3) AS s(r)") == [(1,), (2,), (3,)]

    def test_bare_srf_without_column_alias(self, db):
        assert db("SELECT * FROM generate_series(1, 2) AS s") == [(1,), (2,)]


class TestSrfAsJoinSource:
    def test_join_directly_on_the_function(self, db):
        assert db("SELECT v FROM a JOIN generate_series(1, 2) AS s(r) ON a.k = s.r ORDER BY v") == [
            ("x",),
            ("y",),
        ]

    def test_left_join_keeps_unmatched_rows(self, db):
        assert db(
            "SELECT v, s.r FROM a LEFT JOIN generate_series(1, 2) AS s(r) ON a.k = s.r ORDER BY v"
        ) == [("x", 1), ("y", 2), ("z", None)]

    def test_join_requires_an_alias(self, db):
        with pytest.raises(Exception, match="requires an alias"):
            db("SELECT v FROM a JOIN generate_series(1, 2) ON a.k = 1")


class TestSrfInsideADerivedTable:
    def test_plain_derived_table(self, db):
        assert db("SELECT * FROM (SELECT r FROM generate_series(1, 2) AS s(r)) AS d") == [
            (1,),
            (2,),
        ]

    def test_derived_table_joined_with_on(self, db):
        assert db(
            "SELECT v FROM a JOIN (SELECT r FROM generate_series(1, 2) AS s(r)) AS d "
            "ON a.k = d.r ORDER BY v"
        ) == [("x",), ("y",)]

    def test_derived_table_joined_with_using(self, db):
        assert db(
            "SELECT v FROM a JOIN (SELECT r AS k FROM generate_series(1, 2) AS s(r)) AS d "
            "USING (k) ORDER BY v"
        ) == [("x",), ("y",)]

    def test_derived_table_projecting_a_computed_column(self, db):
        assert db(
            "SELECT d.doubled FROM (SELECT r * 2 AS doubled FROM generate_series(1, 2) AS s(r)) "
            "AS d ORDER BY d.doubled"
        ) == [(2,), (4,)]

    def test_srf_argument_may_read_session_state(self, db):
        """``generate_series(1, array_upper(current_schemas(false), 1))`` — the
        bound is only knowable with a session, so the rows must be produced at
        execution time, not while planning."""
        assert db(
            "SELECT * FROM (select s.r, (current_schemas(false))[s.r] as nspname "
            "from generate_series(1, array_upper(current_schemas(false), 1)) as s(r)) AS d"
        ) == [(1, "public")]


class TestPgjdbcTypeInfoCacheQuery:
    def test_the_whole_query_runs(self, db):
        assert db(TYPEINFO_SQL) == [(False, "b", "int4", 23)]

    def test_typinput_is_exposed(self, db):
        assert db("SELECT typinput FROM pg_catalog.pg_type WHERE typname = 'int4'") == [("int4in",)]

    def test_is_array_is_false_for_a_scalar_type(self, db):
        assert db(
            "SELECT typinput = 'pg_catalog.array_in'::regproc FROM pg_catalog.pg_type "
            "WHERE typname = 'text'"
        ) == [(False,)]
