"""Semantics pinned by the pgjdbc-gauge cluster work: BC era parsing with a
trailing zone offset, nested enum arrays, ungrouped-aggregate one-row
semantics, ``ALTER DATABASE … SET``, ``current_schemas()``, and value-free
Describe of a cast parameter. The pgjdbc suite is the integration oracle;
these pin the engine-level behaviour.

The aggregate expectations were verified against real PostgreSQL 14.13.
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


def rows(storage, session, sql):
    return run(storage, session, sql).rows


class TestBCEraParsing:
    """pgjdbc binds a BC date as ``0101-01-01 BC +00`` — era BEFORE the zone
    offset. PG's datetime input is field-order flexible; ours was not."""

    @pytest.mark.parametrize(
        "lit",
        [
            "0101-01-01 BC +00",
            "0101-01-01 00:00:00 BC +00",
            "0101-01-01 00:00:00+00 BC",
            "0101-01-01 BC",
        ],
    )
    def test_timestamp_accepts_either_order(self, storage, session, lit):
        run(storage, session, "CREATE TABLE ts (id int primary key, v timestamp)")
        run(storage, session, f"INSERT INTO ts VALUES (1, '{lit}')")
        assert rows(storage, session, "SELECT v::text FROM ts")[0][0].endswith(" BC")

    @pytest.mark.parametrize(
        "lit", ["0101-01-01 BC +00", "0101-01-01 00:00:00 BC +00", "0101-01-01 BC"]
    )
    def test_date_keeps_the_era(self, storage, session, lit):
        run(storage, session, "CREATE TABLE d1 (id int primary key, v date)")
        run(storage, session, f"INSERT INTO d1 VALUES (1, '{lit}')")
        assert rows(storage, session, "SELECT v::text FROM d1") == [("0101-01-01 BC",)]


class TestNestedEnumArrays:
    @pytest.fixture(autouse=True)
    def _enum(self, storage, session):
        run(storage, session, "CREATE TYPE flag AS ENUM ('duplicate','new','spike')")

    def test_two_dimensional_enum_array(self, storage, session):
        # Engine level: the value is the nested Python list (the wire layer
        # renders the array literal — asserted separately below).
        got = rows(storage, session, "SELECT '{{duplicate,new},{spike,spike}}'::flag[][]")
        assert got == [([["duplicate", "new"], ["spike", "spike"]],)]

    def test_two_dimensional_renders_as_nested_braces(self):
        # Wire text: nested braces, NOT quoted JSON — the element tag must be
        # inferred from the LEAF, not the outer list.
        from secantus.sql import typemap

        rendered = typemap.to_pg_text([["duplicate", "new"], ["spike", "spike"]], "text[]")
        assert rendered == b"{{duplicate,new},{spike,spike}}"

    def test_one_dimensional_still_works(self, storage, session):
        assert rows(storage, session, "SELECT '{duplicate,new}'::flag[]") == [
            (["duplicate", "new"],)
        ]

    def test_invalid_label_rejected_at_any_depth(self, storage, session):
        with pytest.raises(SQLError) as exc:
            run(storage, session, "SELECT '{{duplicate,nope}}'::flag[][]")
        assert exc.value.sqlstate == "22P02"


class TestUngroupedAggregateOneRow:
    """An ungrouped aggregate yields exactly one row even when WHERE excludes
    everything (verified against real PostgreSQL 14.13)."""

    def test_count_is_zero_not_no_rows(self, storage, session):
        assert rows(storage, session, "SELECT count(*) WHERE 1=2") == [(0,)]

    def test_other_aggregates_are_null(self, storage, session):
        assert rows(storage, session, "SELECT max(3) WHERE 1=2") == [(None,)]
        assert rows(storage, session, "SELECT sum(1) WHERE 1=2") == [(None,)]

    def test_non_aggregate_yields_no_rows(self, storage, session):
        assert rows(storage, session, "SELECT 1 WHERE 1=2") == []

    def test_division_by_zero_surfaces(self, storage, session):
        # pgjdbc's batch tests inject a runtime failure exactly this way.
        with pytest.raises(SQLError) as exc:
            run(storage, session, "SELECT 0/count(*) WHERE 1=2")
        assert exc.value.sqlstate == "22012"

    def test_aggregate_over_the_implicit_row_unchanged(self, storage, session):
        assert rows(storage, session, "SELECT count(*)") == [(1,)]


class TestCurrentSchemas:
    def test_include_implicit(self, storage, session):
        assert rows(storage, session, "SELECT current_schemas(true)") == [
            (["pg_catalog", "public"],)
        ]

    def test_without_implicit(self, storage, session):
        assert rows(storage, session, "SELECT current_schemas(false)") == [(["public"],)]

    def test_any_in_where(self, storage, session):
        got = rows(
            storage,
            session,
            "SELECT n.nspname FROM pg_namespace n "
            "WHERE n.nspname = ANY(current_schemas(true)) ORDER BY 1",
        )
        assert got == [("pg_catalog",), ("public",)]


class TestAlterDatabaseSet:
    def test_set_and_reset(self, storage, session):
        catalog = Catalog(storage)
        run(
            storage,
            session,
            "ALTER DATABASE testdb SET default_transaction_isolation TO 'serializable'",
        )
        assert catalog.db_settings(DB)["default_transaction_isolation"] == "serializable"
        # PG applies it to NEW sessions only — the current one is untouched.
        assert session.get_setting("default_transaction_isolation") == "read committed"
        fresh = Session(database=DB)
        fresh.apply_database_defaults(catalog.db_settings(DB))
        assert fresh.get_setting("default_transaction_isolation") == "serializable"
        run(
            storage,
            session,
            "ALTER DATABASE testdb SET default_transaction_isolation TO DEFAULT",
        )
        assert "default_transaction_isolation" not in catalog.db_settings(DB)

    def test_reset_all(self, storage, session):
        run(storage, session, "ALTER DATABASE testdb SET statement_timeout TO '5s'")
        run(storage, session, "ALTER DATABASE testdb RESET ALL")
        assert Catalog(storage).db_settings(DB) == {}

    def test_unknown_database(self, storage, session):
        with pytest.raises(SQLError) as exc:
            run(storage, session, "ALTER DATABASE nosuch SET statement_timeout TO '5s'")
        assert exc.value.sqlstate == "3D000"

    def test_explicit_session_setting_wins(self, storage, session):
        run(storage, session, "ALTER DATABASE testdb SET statement_timeout TO '5s'")
        fresh = Session(database=DB)
        fresh.settings["statement_timeout"] = "9s"
        fresh.apply_database_defaults(Catalog(storage).db_settings(DB))
        assert fresh.get_setting("statement_timeout") == "9s"


class TestDescribeCastParameter:
    """Describe must not need parameter VALUES: ``SELECT $1::inet`` has a
    shape fixed by the cast target (pgjdbc's PGobject round-trip)."""

    @pytest.mark.parametrize(
        ("sql", "name", "oid"),
        [("SELECT $1::box", "box", 603), ("SELECT $1::inet", "inet", 869)],
    )
    def test_cast_parameter_describes(self, storage, session, sql, name, oid):
        from secantus.sql import engine, planner

        stmt = planner.parse(sql)[0]
        cols = engine.describe_statement(storage, DB, stmt, session, Catalog(storage))
        assert cols is not None
        assert [(c.name, c.pg_oid) for c in cols] == [(name, oid)]

    def test_untyped_parameter_defers_to_execute(self, storage, session):
        from secantus.sql import engine, planner

        stmt = planner.parse("SELECT $1")[0]
        assert engine.describe_statement(storage, DB, stmt, session, Catalog(storage)) is None
