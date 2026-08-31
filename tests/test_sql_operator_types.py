"""Operator result types and undefined-operator errors, over the real wire.

Three fixes share this file because they share a failure mode: the answer was
plausible and the *type* was wrong, which only a driver reading the declared OID
can see. `test_sql_result_type_tags.py` is the sibling for jsonb and
unknown-literal arithmetic; `test_sql_typecheck.py` owns the column-typed half
of the 42883 rule, which needs a catalog this file's constant expressions
deliberately do not have.
"""

from __future__ import annotations

import pytest

import pg_oracle
from secantus.sql import run_sql
from secantus.sql.pgserver import SecantusPGServer
from secantus.sql.session import Session
from secantus.storage import Storage

psycopg = pytest.importorskip("psycopg")

DB = "optypes"

#: Postgres type OIDs the assertions below name.
REGTYPE, TEXT, OID, INTERVAL = 2206, 25, 26, 1186


@pytest.fixture
def storage(tmp_path):
    st = Storage(str(tmp_path))
    try:
        yield st
    finally:
        st.close()


@pytest.fixture
def wire(storage):
    srv = SecantusPGServer(port=0, storage=storage)
    srv.start()
    host, port = srv.address
    conn = psycopg.connect(host=host, port=port, dbname=DB, user="joe", autocommit=True)
    try:
        yield conn
    finally:
        conn.close()
        srv.stop()


def one(conn, sql):
    """``(oid, value)`` of a single-column, single-row query."""
    with conn.cursor() as cur:
        cur.execute(sql)
        return cur.description[0].type_code, cur.fetchall()[0][0]


def tag(storage, sql):
    res = run_sql(storage, DB, sql, session=Session(database=DB, user="secantus"))[-1]
    return res.columns[0].type_tag


class TestPgTypeofIsRegtype:
    """``pg_typeof`` returns **regtype** (2206), not text. Every call wired as
    text: the value was right and the declared type wrong, so this was invisible
    to any check that only compared values."""

    @pytest.mark.parametrize(
        "sql,expected",
        [
            ("select pg_typeof(1)", "integer"),
            ("select pg_typeof(1.5)", "numeric"),
            ("select pg_typeof('a')", "unknown"),
            ("select pg_typeof(true)", "boolean"),
            ("select pg_typeof(now())", "timestamp with time zone"),
            ("select pg_typeof('[1,2]'::jsonb || '[3]'::jsonb)", "jsonb"),
        ],
    )
    def test_pg_typeof_wires_as_regtype(self, wire, sql, expected):
        assert one(wire, sql) == (REGTYPE, expected)

    def test_an_explicit_regtype_cast_too(self, wire):
        assert one(wire, "select 'int4'::regtype") == (REGTYPE, "integer")
        assert one(wire, "select 'integer'::regtype") == (REGTYPE, "integer")

    def test_the_derived_forms_are_unchanged(self, wire):
        """``::text`` and ``::oid`` resolve through the regtype value as before —
        the marker steers typing only, so these must not move."""
        assert one(wire, "select pg_typeof(1)::text") == (TEXT, "integer")
        assert one(wire, "select pg_typeof(1)::oid") == (OID, 23)
        assert one(wire, "select 'int4'::regtype::oid") == (OID, 23)

    def test_regtype_still_compares(self, wire):
        assert one(wire, "select pg_typeof(1) = 'integer'::regtype")[1] is True

    def test_planner_tag(self, storage):
        assert tag(storage, "select pg_typeof(1)") == "regtype"


class TestIntervalArithmetic:
    """``interval '1 day' + 1`` is 42883 in Postgres. We answered
    ``1 day 00:00:01`` — reading the ``1`` as one *second* — because sqlglot
    absorbs a following NUMBER into its multi-part interval form
    (``INTERVAL '1' DAY '2' HOUR``), which Postgres does not have."""

    @pytest.mark.parametrize(
        "sql", ["select interval '1 day' + 1", "select interval '1 day' + 1 + 2"]
    )
    def test_interval_plus_number_is_undefined(self, wire, sql):
        with pytest.raises(psycopg.errors.UndefinedFunction) as e:
            one(wire, sql)
        assert e.value.diag.sqlstate == "42883"
        assert "interval + integer" in str(e.value)

    def test_interval_minus_number_is_undefined(self, wire):
        """Separately broken: an interval rides as a tagged subdocument, so the
        ``jsonb - key`` branch claimed it and answered ``22023 cannot delete
        from scalar`` — a *jsonb* error for an interval."""
        with pytest.raises(psycopg.errors.UndefinedFunction) as e:
            one(wire, "select interval '1 day' - 1")
        assert e.value.diag.sqlstate == "42883"
        assert "interval - integer" in str(e.value)

    def test_interval_plus_unknown_literal_still_resolves(self, wire):
        """``+ 'string'`` is deliberately untouched: PG resolves the unknown to
        an interval, which is what the answer already was."""
        assert str(one(wire, "select interval '1 day' + '2'")[1]) == "1 day, 0:00:02"

    def test_multiplication_by_a_number_still_works(self, wire):
        assert str(one(wire, "select interval '1 day' * 2")[1]) == "2 days, 0:00:00"
        assert str(one(wire, "select interval '1 day' * '2'")[1]) == "2 days, 0:00:00"

    def test_interval_plus_interval_still_works(self, wire):
        oid, value = one(wire, "select interval '1 day' + interval '2 hours'")
        assert (oid, str(value)) == (INTERVAL, "1 day, 2:00:00")

    @pytest.mark.parametrize(
        "sql,expected",
        [
            ("select interval '1 day'", "1 day, 0:00:00"),
            ("select interval '1' day", "1 day, 0:00:00"),
            ("select interval '1 day 2 hours'", "1 day, 2:00:00"),
        ],
    )
    def test_interval_literals_still_parse(self, wire, sql, expected):
        """The parser change must not disturb the interval literal forms."""
        assert str(one(wire, sql)[1]) == expected


class TestTypedTextHasNoArithmetic:
    """Postgres defines no arithmetic operator on text, so a *typed* operand is
    42883 whatever it contains — ``'1'::text + 1`` errors exactly as
    ``'a'::text + 1`` does. We coerced both, so the first silently answered 2
    and the second reported the coercion's 22P02."""

    @pytest.mark.parametrize(
        "sql,message",
        [
            ("select '1'::text + 1", "text + integer"),
            ("select 'a'::text - 1", "text - integer"),
            ("select '1'::text * 2", "text * integer"),
            ("select '1'::text / 1", "text / integer"),
            ("select 1 + '1'::text", "integer + text"),
            ("select '1'::varchar + 1", "character varying + integer"),
            ("select 'a'::char + 1", "character + integer"),
        ],
    )
    def test_typed_text_arithmetic_is_undefined(self, wire, sql, message):
        with pytest.raises(psycopg.errors.UndefinedFunction) as e:
            one(wire, sql)
        assert e.value.diag.sqlstate == "42883"
        assert message in str(e.value)

    def test_an_unknown_literal_still_coerces(self, wire):
        """The whole point of the distinction: pgbench binds parameters
        typeless, and an unknown literal is not a typed operand."""
        assert one(wire, "select '1' + 1")[1] == 2

    def test_casting_out_of_text_is_the_supported_form(self, wire):
        assert one(wire, "select '1'::text::int + 1")[1] == 2

    def test_concatenation_is_not_arithmetic(self, wire):
        assert one(wire, "select 'a'::text || 'b'") == (TEXT, "ab")


def _pg_reference():
    """A live PostgreSQL to check against, or None. Point elsewhere with
    SECANTUS_PG_ORACLE_DSN.

    Delegates to `pg_oracle` so all six oracle suites share one probe, and one
    skip reason that says why. The inline copies this replaced had drifted to
    three different default DSNs and skipped with a message indistinguishable
    from "PostgreSQL is not installed".
    """
    return pg_oracle.connect()


#: Every shape here diverged before 2026-08-30.
_SHAPES = [
    "select pg_typeof(1)",
    "select pg_typeof('a')",
    "select pg_typeof(1.5)",
    "select pg_typeof(true)",
    "select pg_typeof(now())",
    "select 'int4'::regtype",
    "select pg_typeof(1)::text",
    "select pg_typeof(1)::oid",
    "select interval '1 day' + 1",
    "select interval '1 day' - 1",
    "select interval '1 day' + 1 + 2",
    "select interval '1 day' + '2'",
    "select interval '1 day' * 2",
    "select interval '1 day' + interval '2 hours'",
    "select interval '1 day 2 hours'",
    "select '1'::text + 1",
    "select 'a'::text - 1",
    "select 1 + '1'::text",
    "select '1'::varchar + 1",
    "select 'a'::char + 1",
    "select '1' + 1",
    "select '1'::text::int + 1",
]


@pytest.mark.skipif(not pg_oracle.available(), reason=pg_oracle.skip_reason())
def test_operator_types_match_real_postgres(wire):
    """The assertions above say what we believe; this says what PostgreSQL does.

    Compares the OID and the SQLSTATE as well as the value — the bugs here were
    a right answer under a wrong declared type, and a wrong error code."""
    pg = _pg_reference()
    assert pg is not None

    def probe(conn, sql):
        try:
            with conn.cursor() as cur:
                cur.execute(sql)
                return (cur.description[0].type_code, str(cur.fetchall()[0][0]))
        except psycopg.Error as e:
            return ("error", e.diag.sqlstate)

    try:
        mismatches = [
            (sql, probe(pg, sql), probe(wire, sql))
            for sql in _SHAPES
            if probe(pg, sql) != probe(wire, sql)
        ]
    finally:
        pg.close()
    assert not mismatches, "\n".join(f"{s}: pg={t} us={u}" for s, t, u in mismatches)
