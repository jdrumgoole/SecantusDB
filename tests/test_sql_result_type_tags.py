"""A computed column's DECLARED type must match the value it sends.

Every bug pinned here presented the same way: the value was right, the declared
OID came from the wrong operand, and the **client** raised while decoding —
``psycopg`` calling ``int('{1,3}')`` on a jsonb payload wired under an int4 oid.
That is invisible to a value-only comparison, and invisible to the in-process
engine, so these tests go over the real wire and assert the OID as well as the
value. `test_sql_jsonb.py` covers the operators' semantics; this file covers
their typing.
"""

from __future__ import annotations

import os

import pytest

from secantus.sql import run_sql
from secantus.sql.pgserver import SecantusPGServer
from secantus.sql.session import Session
from secantus.storage import Storage

psycopg = pytest.importorskip("psycopg")

DB = "typedb"

#: Postgres type OIDs the assertions below name.
JSONB, INT4, TEXT, NUMERIC = 3802, 23, 25, 1700


@pytest.fixture
def storage(tmp_path):
    st = Storage(str(tmp_path))
    try:
        yield st
    finally:
        st.close()


@pytest.fixture
def wire(storage):
    """A connection to our own PG-wire server — the only view that sees the
    declared OID the way a driver does."""
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
    """The internal type tag the planner assigns the first output column."""
    res = run_sql(storage, DB, sql, session=Session(database=DB, user="secantus"))[-1]
    return res.columns[0].type_tag


class TestJsonbOperatorsKeepTheirType:
    """``jsonb || jsonb`` and ``jsonb - key`` answer jsonb. Before this, ``||``
    typed as text and ``-`` typed from its RIGHT operand (int4 / numeric), so a
    jsonb payload went out under a numeric oid and psycopg raised
    ``invalid literal for int() with base 10: '{1,3}'`` — a hard client-side
    failure for every ``jsonb - anything``."""

    @pytest.mark.parametrize(
        "sql,expected",
        [
            ("select '[1,2]'::jsonb || '[3]'::jsonb", [1, 2, 3]),
            ("select '{\"x\":1}'::jsonb || '[3]'::jsonb", [{"x": 1}, 3]),
            ("select '[1,2]'::jsonb || '\"s\"'::jsonb", [1, 2, "s"]),
            ("select '[1,2]'::jsonb || '5'::jsonb", [1, 2, 5]),
            ('select \'{"x":1,"y":2}\'::jsonb || \'{"y":9}\'::jsonb', {"x": 1, "y": 9}),
        ],
    )
    def test_concat_is_jsonb(self, wire, sql, expected):
        assert one(wire, sql) == (JSONB, expected)

    @pytest.mark.parametrize(
        "sql,expected",
        [
            ("select '[1,2,3]'::jsonb - 1", [1, 3]),
            ("select '[1,2,3]'::jsonb - 0", [2, 3]),
            ("select '[\"a\",\"b\"]'::jsonb - 'a'", ["b"]),
            ("select '{\"x\":1}'::jsonb - 'x'", {}),
            ("select '{\"x\":1,\"y\":2}'::jsonb - array['x','y']", {}),
        ],
    )
    def test_delete_is_jsonb(self, wire, sql, expected):
        """These four raised in the CLIENT before the fix — the server answered
        correctly and declared the wrong type."""
        assert one(wire, sql) == (JSONB, expected)

    def test_nested_operators_stay_jsonb(self, wire):
        assert one(wire, "select ('[1,2,3]'::jsonb - 1) || '[9]'::jsonb") == (JSONB, [1, 3, 9])

    def test_a_jsonb_returning_function_feeds_concat(self, wire):
        assert one(wire, "select jsonb_build_array(1,2) || '[3]'::jsonb") == (JSONB, [1, 2, 3])

    def test_the_result_is_usable_as_jsonb(self, wire):
        """The tag has to be real, not cosmetic: jsonb operators must accept it."""
        assert one(wire, "select ('[1,2]'::jsonb || '[3]'::jsonb) -> 0") == (JSONB, 1)
        assert one(wire, "select jsonb_array_length('[1,2]'::jsonb || '[3]'::jsonb)") == (INT4, 3)

    @pytest.mark.parametrize(
        "sql",
        [
            "select '[1,2]'::jsonb || '[3]'::jsonb",
            "select '[1,2,3]'::jsonb - 1",
            "select '{\"x\":1}'::jsonb - 'x'",
            "select '{\"a\":1}'::jsonb #- '{a}'",
        ],
    )
    def test_planner_tag(self, storage, sql):
        assert tag(storage, sql) == "json"

    def test_text_concat_is_untouched(self, wire):
        """The jsonb rule must not swallow ordinary ``text || text``."""
        assert one(wire, "select 'a' || 'b'") == (TEXT, "ab")


class TestUnknownLiteralsCoerceToTheOtherOperand:
    """Postgres resolves an ``unknown`` literal to the OTHER operand's type
    *before* choosing an operator, so the target type decides both the parse and
    the error. We widened instead — ``'1.5' + 1`` answered 2.5 under the int4 oid
    the literal ``1`` had already fixed, and the client raised."""

    def test_integral_text_still_coerces(self, wire):
        """pgbench binds every parameter typeless; this path must keep working."""
        assert one(wire, "select '1' + 1") == (INT4, 2)
        assert one(wire, "select 1 + '1'") == (INT4, 2)
        assert one(wire, "select '1' - 1") == (INT4, 0)

    @pytest.mark.parametrize("sql", ["select '1.5' + 1", "select 'a' + 1", "select '1e3' + 1"])
    def test_non_integral_text_is_invalid_integer_input(self, wire, sql):
        with pytest.raises(psycopg.errors.InvalidTextRepresentation) as e:
            one(wire, sql)
        assert e.value.diag.sqlstate == "22P02"
        assert "type integer" in str(e.value)

    def test_a_date_shaped_literal_is_integer_input_too(self, wire):
        """``'2020-01-01' + 1`` is *integer* input in PG, not date arithmetic.
        We answered '2020-01-02' under an int4 oid, so the client raised."""
        with pytest.raises(psycopg.errors.InvalidTextRepresentation):
            one(wire, "select '2020-01-01' + 1")

    def test_a_typed_date_still_does_date_arithmetic(self, wire):
        """Only a bare *literal* is unknown — a cast is typed and keeps its
        date arithmetic."""
        _, value = one(wire, "select '2020-01-01'::date + 1")
        assert str(value) == "2020-01-02"

    def test_a_date_column_still_does_date_arithmetic(self, wire):
        with wire.cursor() as cur:
            cur.execute("CREATE TABLE d (c date)")
            cur.execute("INSERT INTO d VALUES ('2020-01-01')")
            cur.execute("SELECT c + 1 FROM d")
            assert str(cur.fetchall()[0][0]) == "2020-01-02"

    def test_beside_an_interval_the_unknown_becomes_an_interval(self, wire):
        """Coercion is to the other operand's type, and that type is not always
        a number: beside an interval the literal must parse as an *interval*.
        ``'2020-01-01'`` does not, so PG answers 22007 — we read it as a date and
        answered a timestamp under the interval oid, and psycopg raised
        ``can't parse interval '2020-01-02 00:00:00'``."""
        with pytest.raises(psycopg.errors.InvalidDatetimeFormat) as e:
            one(wire, "select '2020-01-01' + interval '1 day'")
        assert e.value.diag.sqlstate == "22007"
        assert "type interval" in str(e.value)

    def test_an_interval_shaped_literal_beside_an_interval_still_adds(self, wire):
        """The coercion has to actually work, not just reject."""
        _, value = one(wire, "select '10:00' + interval '1 hour'")
        assert str(value) == "11:00:00"

    def test_multiplication_resolves_the_unknown_to_a_number_instead(self, wire):
        """``*`` / ``/`` pick float8 for the unknown, not interval — so
        ``interval '1 day' * '2'`` is two days, not one day plus two seconds."""
        _, value = one(wire, "select interval '1 day' * '2'")
        assert str(value) == "2 days, 0:00:00"


class TestBooleanHasNoArithmetic:
    """Postgres defines no arithmetic operator on boolean. Python would not
    raise for us — ``bool`` IS an ``int`` — so ``true + 1`` quietly answered 2."""

    @pytest.mark.parametrize(
        "sql", ["select true + 1", "select 1 + true", "select true - false", "select true * 2"]
    )
    def test_boolean_arithmetic_is_undefined(self, wire, sql):
        with pytest.raises(psycopg.errors.UndefinedFunction) as e:
            one(wire, sql)
        assert e.value.diag.sqlstate == "42883"
        assert "boolean" in str(e.value)


def _pg_reference():
    """A live PostgreSQL to check against, or None. Point elsewhere with
    SECANTUS_PG_ORACLE_DSN."""
    dsn = os.environ.get(
        "SECANTUS_PG_ORACLE_DSN", "host=127.0.0.1 port=5432 dbname=postgres user=jdrumgoole"
    )
    try:
        return psycopg.connect(dsn, autocommit=True, connect_timeout=3)
    except Exception:
        return None


#: Shapes whose answer AND declared type are compared against a real server.
#: Every one of these diverged before 2026-08-30.
_SHAPES = [
    "select '[1,2]'::jsonb || '[3]'::jsonb",
    "select '{\"x\":1}'::jsonb || '[3]'::jsonb",
    "select '[1,2]'::jsonb || '\"s\"'::jsonb",
    'select \'{"x":1,"y":2}\'::jsonb || \'{"y":9}\'::jsonb',
    "select '[1,2,3]'::jsonb - 1",
    "select '[\"a\",\"b\"]'::jsonb - 'a'",
    "select '{\"x\":1}'::jsonb - 'x'",
    "select '{\"x\":1,\"y\":2}'::jsonb - array['x','y']",
    "select ('[1,2,3]'::jsonb - 1) || '[9]'::jsonb",
    "select jsonb_build_array(1,2) || '[3]'::jsonb",
    "select '1' + 1",
    "select 1 + '1'",
    "select '1.5' + 1",
    "select 'a' + 1",
    "select '2020-01-01' + 1",
    "select true + 1",
    "select true - false",
    "select '2020-01-01' + interval '1 day'",
    "select '10:00' + interval '1 hour'",
    "select interval '1 day' * '2'",
]


@pytest.mark.skipif(_pg_reference() is None, reason="no local PostgreSQL reference server")
def test_result_types_match_real_postgres(wire):
    """The assertions above say what we believe; this says what PostgreSQL does.

    Compares the OID as well as the value — the whole class of bug here is a
    right answer under a wrong declared type, which a value comparison cannot
    see."""
    pg = _pg_reference()
    assert pg is not None

    def probe(conn, sql):
        try:
            with conn.cursor() as cur:
                cur.execute(sql)
                return (cur.description[0].type_code, cur.fetchall()[0][0])
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
