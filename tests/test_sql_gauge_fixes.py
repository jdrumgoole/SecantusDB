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
        assert rows(storage, session, "SELECT 5.52 / CAST(2.4 AS NUMERIC(10, 2))") == [(2.3,)]


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
