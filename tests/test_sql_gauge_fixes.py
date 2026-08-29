"""Semantics pinned by the SQLAlchemy compliance-gauge fixes (77.5% → 97%):
LIKE ESCAPE, IS [NOT] DISTINCT FROM, numeric-cast division, LIMIT/OFFSET
expressions, INSERT DEFAULT VALUES, CREATE SEQUENCE NO MINVALUE/MAXVALUE,
typmod + default + view-column + constraint-comment reflection, quoted
identifiers in pg_get_constraintdef, declared composite-PK order, and the
temp-table reflection surface. The compliance suite is the integration
oracle; these pin the engine-level behavior.
"""

from __future__ import annotations

from decimal import Decimal

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


class TestLikeEscape:
    @pytest.fixture(autouse=True)
    def _table(self, storage, session):
        run(storage, session, "CREATE TABLE t (id int primary key, data varchar(50))")
        run(storage, session, "INSERT INTO t VALUES (1,'abcde'), (2,'ab%c'), (3,'e_fg')")

    def test_escape_literal_percent(self, storage, session):
        assert rows(storage, session, "SELECT id FROM t WHERE data LIKE 'ab/%c' ESCAPE '/'") == [
            (2,)
        ]

    def test_escape_literal_underscore(self, storage, session):
        assert rows(storage, session, "SELECT id FROM t WHERE data LIKE 'e#_fg' ESCAPE '#'") == [
            (3,)
        ]

    def test_computed_pattern(self, storage, session):
        assert rows(
            storage,
            session,
            "SELECT id FROM t WHERE data LIKE '%' || CAST('b%c' AS VARCHAR) ESCAPE '#'",
        ) == [(2,)]


class TestIsDistinctFrom:
    def test_null_vs_null_not_distinct(self, storage, session):
        assert rows(storage, session, "SELECT 1 WHERE NULL IS NOT DISTINCT FROM NULL") == [(1,)]

    def test_value_vs_null_distinct(self, storage, session):
        assert rows(storage, session, "SELECT 1 WHERE 5 IS DISTINCT FROM NULL") == [(1,)]

    def test_columns(self, storage, session):
        run(storage, session, "CREATE TABLE d (id int primary key, a int, b int)")
        run(storage, session, "INSERT INTO d VALUES (1, 5, 5), (2, 5, 6), (3, NULL, NULL)")
        assert rows(storage, session, "SELECT id FROM d WHERE a IS DISTINCT FROM b") == [(2,)]
        assert rows(
            storage, session, "SELECT id FROM d WHERE a IS NOT DISTINCT FROM b ORDER BY id"
        ) == [(1,), (3,)]


class TestNumericArithmetic:
    def test_int_cast_to_numeric_divides_exactly(self, storage, session):
        assert rows(storage, session, "SELECT CAST(15 AS NUMERIC) / 10") == [(Decimal("1.5"),)]

    def test_bare_int_division_still_truncates(self, storage, session):
        assert rows(storage, session, "SELECT 15 / 10") == [(1,)]

    def test_float_by_numeric_typmod(self, storage, session):
        # numeric division at PG's derived result scale (select_div_scale):
        # real Postgres 14.13 reports 2.3000000000000000 — scale 16 — and so
        # do we now (the former value-only assertion is upgraded to pin the
        # scale; the full case battery lives in test_sql_numeric_div_scale.py).
        got = rows(storage, session, "SELECT 5.52 / CAST(2.4 AS NUMERIC(10, 2))")
        assert str(got[0][0]) == "2.3000000000000000"


class TestLimitOffsetExpressions:
    @pytest.fixture(autouse=True)
    def _table(self, storage, session):
        run(storage, session, "CREATE TABLE t (id int primary key)")
        run(storage, session, "INSERT INTO t VALUES (1),(2),(3),(4)")

    def test_offset_expression(self, storage, session):
        assert rows(storage, session, "SELECT id FROM t ORDER BY id LIMIT 2 OFFSET 1 + 1") == [
            (3,),
            (4,),
        ]

    def test_limit_cast(self, storage, session):
        assert rows(storage, session, "SELECT id FROM t ORDER BY id LIMIT CAST(2 AS INT)") == [
            (1,),
            (2,),
        ]


class TestInsertDefaultValues:
    def test_default_values_with_returning(self, storage, session):
        run(storage, session, "CREATE TABLE ap (id SERIAL PRIMARY KEY, n int DEFAULT 7)")
        res = run(storage, session, "INSERT INTO ap DEFAULT VALUES RETURNING id, n")
        assert res.rows == [(1, 7)]


class TestSequenceNoBounds:
    def test_no_minvalue_no_maxvalue(self, storage, session):
        res = run(storage, session, "CREATE SEQUENCE s NO MINVALUE NO MAXVALUE")
        assert res.command_tag == "CREATE SEQUENCE"
        assert rows(storage, session, "SELECT nextval('s')") == [(1,)]


class TestReflectionSurfaces:
    def test_varchar_typmod_reflects(self, storage, session):
        run(storage, session, "CREATE TABLE vt (id int primary key, name varchar(52))")
        got = rows(
            storage,
            session,
            "SELECT a.atttypid, a.atttypmod FROM pg_attribute a "
            "JOIN pg_class c ON a.attrelid = c.oid "
            "WHERE c.relname = 'vt' AND a.attname = 'name'",
        )
        assert got == [(1043, 56)]

    def test_format_type_renders_typmod(self, storage, session):
        run(storage, session, "CREATE TABLE ft (id int primary key, name varchar(52))")
        got = rows(
            storage,
            session,
            "SELECT format_type(a.atttypid, a.atttypmod) FROM pg_attribute a "
            "JOIN pg_class c ON a.attrelid = c.oid "
            "WHERE c.relname = 'ft' AND a.attname = 'name'",
        )
        assert got == [("character varying(52)",)]

    def test_serial_default_via_pg_get_expr(self, storage, session):
        run(storage, session, "CREATE TABLE st (id SERIAL PRIMARY KEY)")
        got = rows(
            storage,
            session,
            "SELECT pg_get_expr(ad.adbin, ad.adrelid) FROM pg_attrdef ad "
            "JOIN pg_class c ON ad.adrelid = c.oid WHERE c.relname = 'st'",
        )
        assert got == [("nextval('st_id_seq'::regclass)",)]

    def test_view_columns_in_pg_attribute(self, storage, session):
        run(storage, session, "CREATE TABLE bt (id int primary key, data text)")
        run(storage, session, "CREATE VIEW bv AS SELECT * FROM bt")
        got = rows(
            storage,
            session,
            "SELECT a.attname FROM pg_attribute a JOIN pg_class c ON a.attrelid = c.oid "
            "WHERE c.relname = 'bv' ORDER BY a.attnum",
        )
        assert got == [("id",), ("data",)]

    def test_constraint_comment_in_pg_description(self, storage, session):
        run(
            storage,
            session,
            "CREATE TABLE ct (id int primary key, n int CONSTRAINT n_pos CHECK (n > 0))",
        )
        run(storage, session, "COMMENT ON CONSTRAINT n_pos ON ct IS 'positive'")
        got = rows(
            storage,
            session,
            "SELECT d.description FROM pg_description d "
            "JOIN pg_constraint con ON d.objoid = con.oid WHERE con.conname = 'n_pos'",
        )
        assert got == [("positive",)]

    def test_fk_condef_quotes_bizarro_identifiers(self, storage, session):
        run(storage, session, 'CREATE TABLE "ref table" ("pk col" int primary key)')
        run(
            storage,
            session,
            'CREATE TABLE src (id int primary key, "weird %col" int, '
            'CONSTRAINT wfk FOREIGN KEY ("weird %col") REFERENCES "ref table" ("pk col"))',
        )
        got = rows(
            storage,
            session,
            "SELECT pg_get_constraintdef(con.oid) FROM pg_constraint con WHERE con.conname = 'wfk'",
        )
        assert got == [('FOREIGN KEY ("weird %col") REFERENCES "ref table"("pk col")',)]

    def test_composite_pk_declared_order(self, storage, session):
        run(
            storage,
            session,
            "CREATE TABLE ck (id int, attr int, name varchar(20), "
            "CONSTRAINT ck_pk PRIMARY KEY (name, id, attr))",
        )
        table = Catalog(storage).get(DB, "ck")
        assert [c.name for c in table.ordered_pk_columns()] == ["name", "id", "attr"]

    def test_temp_table_hidden_from_pg_class_listing(self, storage, session):
        run(storage, session, "CREATE TABLE perm (id int primary key)")
        run(storage, session, "CREATE TEMP TABLE tmp_t (id int primary key)")
        got = rows(
            storage,
            session,
            "SELECT relname FROM pg_class WHERE relkind = 'r' AND relpersistence != 't'",
        )
        assert ("perm",) in got and ("tmp_t",) not in got

    def test_multichar_escape_errors(self, storage, session):
        run(storage, session, "CREATE TABLE le (id int primary key, data text)")
        run(storage, session, "INSERT INTO le VALUES (1, 'x')")
        with pytest.raises(SQLError) as exc:
            run(storage, session, "SELECT id FROM le WHERE data LIKE 'x' ESCAPE 'ab'")
        assert exc.value.sqlstate == "22025"


class TestFromlessAndDerived:
    """Round two of the gauge fixes: FROM-less EXISTS, parenthesized union
    arms, set-op / VALUES / FROM-less derived tables, ordered scalar
    subqueries, nextval in VALUES, INCLUDE covering indexes."""

    @pytest.fixture(autouse=True)
    def _table(self, storage, session):
        run(storage, session, "CREATE TABLE st (id int primary key, x int)")
        run(storage, session, "INSERT INTO st VALUES (1, 10), (2, 20), (3, 30)")

    def test_fromless_where_exists(self, storage, session):
        assert rows(storage, session, "SELECT 1 WHERE EXISTS (SELECT * FROM st)") == [(1,)]
        assert rows(storage, session, "SELECT 1 WHERE EXISTS (SELECT * FROM st WHERE x = 99)") == []

    def test_parenthesized_union_arms_with_limit(self, storage, session):
        assert rows(
            storage,
            session,
            "(SELECT id FROM st ORDER BY id LIMIT 1) UNION "
            "(SELECT id FROM st ORDER BY id DESC LIMIT 1) ORDER BY id",
        ) == [(1,), (3,)]

    def test_setop_derived_table(self, storage, session):
        assert rows(
            storage,
            session,
            "SELECT a.id FROM (SELECT id FROM st WHERE id = 2 "
            "UNION SELECT id FROM st WHERE id = 3) AS a ORDER BY a.id",
        ) == [(2,), (3,)]

    def test_values_derived_table(self, storage, session):
        assert rows(storage, session, "SELECT v.a FROM (VALUES (2), (1)) AS v(a) ORDER BY v.a") == [
            (1,),
            (2,),
        ]

    def test_fromless_derived_table(self, storage, session):
        assert rows(storage, session, "SELECT a.x FROM (SELECT 1 AS x) AS a") == [(1,)]

    def test_scalar_subquery_honors_order_and_limit(self, storage, session):
        assert rows(
            storage,
            session,
            "SELECT (SELECT st.id FROM st ORDER BY st.id DESC LIMIT 1) AS m",
        ) == [(3,)]

    def test_scalar_subquery_multiple_rows_errors(self, storage, session):
        with pytest.raises(SQLError) as exc:
            run(storage, session, "SELECT (SELECT st.id FROM st ORDER BY st.id LIMIT 2) AS m")
        assert exc.value.sqlstate == "21000"

    def test_insert_select_from_aliased_values(self, storage, session):
        run(storage, session, "CREATE TABLE ft (id SERIAL PRIMARY KEY, v float)")
        res = run(
            storage,
            session,
            "INSERT INTO ft (v) SELECT p0::FLOAT FROM (VALUES (1.5, 0), (0.5, 1)) "
            "AS sen(p0, c) ORDER BY c RETURNING ft.id, ft.v",
        )
        assert res.rows == [(1, 1.5), (2, 0.5)]

    def test_nextval_in_insert_values(self, storage, session):
        run(storage, session, "CREATE SEQUENCE tab_seq START 50")
        run(storage, session, "CREATE TABLE sq (id int primary key, d text)")
        res = run(storage, session, "INSERT INTO sq VALUES (nextval('tab_seq'), 'x') RETURNING id")
        assert res.rows == [(50,)]

    def test_covering_index_include_reflection(self, storage, session):
        run(storage, session, "CREATE INDEX st_x ON st (x) INCLUDE (id)")
        got = rows(
            storage,
            session,
            "SELECT i.indnkeyatts, i.indnatts FROM pg_index i "
            "JOIN pg_class c ON i.indexrelid = c.oid WHERE c.relname = 'st_x'",
        )
        assert got == [(1, 2)]


def test_prepared_select_from_view_describe(storage, session):
    """Extended-protocol Describe of a SELECT from a view must resolve the
    expanded column shape — NoData followed by DataRows crashes libpq clients
    (surfaced by the sqllogictest postgres-extended lane)."""
    from secantus.sql import engine, planner
    from secantus.sql.catalog import Catalog

    run(storage, session, "CREATE TABLE t1 (x int primary key)")
    run(storage, session, "INSERT INTO t1 VALUES (0), (1)")
    run(storage, session, "CREATE VIEW v2 AS SELECT x FROM t1 WHERE x = 0")
    stmt = planner.parse("SELECT x FROM v2")[0]
    cols = engine.describe_statement(storage, DB, stmt, session, Catalog(storage))
    assert cols is not None and [c.name for c in cols] == ["x"]
    assert engine.run_statement(storage, DB, stmt, session).rows == [(0,)]


def test_unaliased_cast_column_named_after_typname(storage, session):
    """PG names a bare cast's output column after the target typname —
    ``SELECT 2::int8`` is column ``int8`` (surfaced by the pgtest gauge)."""
    res = run(storage, session, "SELECT 2::int8, 3::int, 'x'::varchar, 4 AS four, 5")
    assert [c.name for c in res.columns] == ["int8", "int4", "varchar", "four", "?column?"]


class TestDescribeCTE:
    """Describe must report a CTE query's real column shape — NoData followed
    by DataRows is a protocol violation pgjdbc rejects outright ("Received
    resultset tuples, but no field structure for them"). Describe stays
    side-effect free even for a data-modifying CTE."""

    def _describe(self, storage, session, sql):
        from secantus.sql import engine, planner
        from secantus.sql.catalog import Catalog

        stmt = planner.parse(sql)[0]
        cols = engine.describe_statement(storage, DB, stmt, session, Catalog(storage))
        return [c.name for c in cols] if cols else None

    @pytest.fixture(autouse=True)
    def _table(self, storage, session):
        run(storage, session, "CREATE TABLE cte_t (a int, str text)")

    def test_plain_cte(self, storage, session):
        assert self._describe(
            storage, session, "WITH y AS (SELECT a FROM cte_t) SELECT a FROM y"
        ) == ["a"]

    def test_column_alias_list(self, storage, session):
        got = self._describe(
            storage, session, "WITH z(p, q) AS (SELECT a, str FROM cte_t) SELECT p, q FROM z"
        )
        assert got == ["p", "q"]

    def test_data_modifying_cte(self, storage, session):
        got = self._describe(
            storage,
            session,
            "WITH x AS (INSERT INTO cte_t(a, str) VALUES (43, 'abc') RETURNING a, str) "
            "SELECT * FROM x",
        )
        assert got == ["a", "str"]

    def test_describe_has_no_side_effects(self, storage, session):
        self._describe(
            storage,
            session,
            "WITH x AS (INSERT INTO cte_t(a, str) VALUES (1, 'x') RETURNING a) SELECT * FROM x",
        )
        assert rows(storage, session, "SELECT count(*) FROM cte_t") == [(0,)]

    def test_execute_still_runs(self, storage, session):
        res = run(
            storage,
            session,
            "WITH x AS (INSERT INTO cte_t(a, str) VALUES (7, 'q') RETURNING a, str) "
            "SELECT * FROM x",
        )
        assert res.rows == [(7, "q")]
