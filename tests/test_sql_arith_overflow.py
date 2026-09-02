"""Arithmetic and cast results Postgres' fixed-width types cannot hold.

Python's `int` is unbounded and its `float` saturates to `inf`, so a computed
value that overflows a SQL type was produced SILENTLY here where PostgreSQL
answers `22003`. For integers that is worse than a wrong error: `i + 1` on an
`int` column answered 2147483648 and sent it under oid 23, a value four bytes
cannot carry, so the column's declared type and its contents disagreed.

`1e39::float4` was an `XX000` internal error — `struct.pack('!f', …)` raising
`OverflowError` straight onto the wire.

Every expectation here was measured against PostgreSQL 14.13.

Still divergent, deliberately: arithmetic inside a **WHERE** clause. It lowers
to a Mongo `$expr` and is evaluated by `secantus/expressions.py`, the operator
engine the MongoDB server shares — teaching it PostgreSQL's integer widths
would break the layer boundary. See `tasks/backlog.md`.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.errors import SQLError
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture()
def db(tmp_path):
    storage = Storage(str(tmp_path))
    session = Session(database="t")

    def run(sql: str):
        return [r.rows for r in run_sql(storage, "t", sql, session=session)]

    run("CREATE TABLE ov (i int, s smallint, b bigint)")
    run("INSERT INTO ov VALUES (2147483647, 32767, 9223372036854775807)")
    run("CREATE TABLE lo (i int, s smallint, b bigint)")
    run("INSERT INTO lo VALUES (-2147483648, -32768, -9223372036854775808)")
    try:
        yield run
    finally:
        storage.close()


def _err(db, sql: str) -> SQLError:
    with pytest.raises(SQLError) as exc:
        db(sql)
    return exc.value


class TestIntegerOverflow:
    """`22003` for a result outside the *declared* integer width."""

    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            # The width comes from the operand types, not from the values.
            ("SELECT i + 1 FROM ov", "integer out of range"),
            ("SELECT i - (-1) FROM ov", "integer out of range"),
            ("SELECT i * 2 FROM ov", "integer out of range"),
            ("SELECT b + 1 FROM ov", "bigint out of range"),
            ("SELECT b * 2 FROM ov", "bigint out of range"),
            ("SELECT s * 2::smallint FROM ov", "smallint out of range"),
            # No FROM at all: the tag used to be inferred only *after* the value
            # had been computed, so these went unchecked while the same
            # expression over a table raised.
            ("SELECT 2147483647 + 1", "integer out of range"),
            ("SELECT 1 + 2147483647", "integer out of range"),
            ("SELECT 2147483647::int + 1", "integer out of range"),
            ("SELECT 32767::smallint * 2::smallint", "smallint out of range"),
            ("SELECT 9223372036854775807 + 1", "bigint out of range"),
            # Division overflows at exactly one point, because the range is
            # asymmetric: -INT_MIN has no int4 answer.
            ("SELECT (-2147483648)::int / (-1)", "integer out of range"),
            ("SELECT (-9223372036854775808)::bigint / (-1)", "bigint out of range"),
            # Same asymmetry, reached by the two unary operators.
            ("SELECT abs((-2147483648)::int)", "integer out of range"),
            ("SELECT abs(i) FROM lo", "integer out of range"),
            ("SELECT -i FROM lo", "integer out of range"),
        ],
    )
    def test_overflow(self, db, sql, want):
        exc = _err(db, sql)
        assert exc.sqlstate == "22003"
        assert exc.message == want

    @pytest.mark.parametrize(
        "sql",
        [
            # A cast reports its TARGET type and never looks at the operand, so
            # the inner int4 addition went unstamped and answered 2147483648.
            "SELECT (2147483647 + 1)::bigint",
            "SELECT coalesce(2147483647 + 1, 0)",
            "SELECT CASE WHEN true THEN 2147483647 + 1 END",
        ],
    )
    def test_overflow_under_a_wrapper(self, db, sql):
        assert _err(db, sql).sqlstate == "22003"

    @pytest.mark.parametrize(
        "sql",
        [
            "SELECT (SELECT i + 1 FROM ov)",
            "SELECT x FROM (SELECT i + 1 AS x FROM ov) t",
            "WITH t AS (SELECT i + 1 AS x FROM ov) SELECT * FROM t",
            "SELECT i + 1 FROM ov GROUP BY i",
            "SELECT i + 1 FROM ov ORDER BY 1",
            "SELECT max(i) + 1 FROM ov",
            "INSERT INTO ov (i) VALUES (2147483647 + 1)",
            "UPDATE ov SET b = b + 1",
        ],
    )
    def test_overflow_reaches_every_clause(self, db, sql):
        assert _err(db, sql).sqlstate == "22003"


class TestIntegerPromotion:
    """The width is Postgres' promotion of the *operand* types — widening is
    not overflow, and reading the width off the values could not tell the two
    apart."""

    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            # smallint ⊕ integer-literal is int4 arithmetic: 32768 is correct.
            ("SELECT s + 1 FROM ov", 32768),
            ("SELECT s * 2 FROM ov", 65534),
            ("SELECT -s FROM ov", -32767),
            # int ⊕ bigint is int8 arithmetic.
            ("SELECT 2147483647 * 2::bigint", 4294967294),
            ("SELECT 2147483647 + 1::bigint", 2147483648),
            ("SELECT 9223372036854775807::bigint * 1::int", 9223372036854775807),
            # `sum()` over int returns bigint, so it does not overflow at int4.
            ("SELECT sum(i) + 1 FROM ov", 2147483648),
            # -INT_MAX is representable; only -INT_MIN is not.
            ("SELECT -i FROM ov", -2147483647),
            ("SELECT (-2147483648)::int % (-1)", 0),
        ],
    )
    def test_no_overflow(self, db, sql, want):
        assert db(sql)[0] == [(want,)]


class TestFloatOverflow:
    """`CHECKFLOATVAL`: an infinite result errors unless an operand was already
    infinite, and a zero result errors unless zero was a legal answer."""

    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT 1e308::float8 * 10", "value out of range: overflow"),
            ("SELECT -1e308::float8 * 10", "value out of range: overflow"),
            ("SELECT 1e308::float8 + 1e308::float8", "value out of range: overflow"),
            # Zero from two non-zero operands is an underflow, not a zero.
            ("SELECT 1e-320::float8 / 1e10", "value out of range: underflow"),
        ],
    )
    def test_float_overflow(self, db, sql, want):
        exc = _err(db, sql)
        assert exc.sqlstate == "22003"
        assert exc.message == want

    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            # An infinite operand makes an infinite result legal.
            ("SELECT 'inf'::float8 + 1", float("inf")),
            ("SELECT 'inf'::float8 * 2", float("inf")),
            # A zero operand makes a zero product legal.
            ("SELECT 1e-320::float8 * 0", 0.0),
        ],
    )
    def test_infinity_is_not_overflow(self, db, sql, want):
        assert db(sql)[0] == [(want,)]

    def test_nan_passes_through(self, db):
        for sql in ("SELECT 'inf'::float8 * 0", "SELECT 'nan'::float8 + 1"):
            (value,) = db(sql)[0][0]
            assert value != value  # NaN


class TestFloatCastRange:
    """Casting to a float type reports the range error in the two spellings PG
    uses, and which one you get is decided by the SOURCE type."""

    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            # From numeric: the input spelled out in plain decimal.
            ("SELECT 1e39::float4", '"1000000000000000000000000000000000000000"'),
            ("SELECT (-1e39)::float4", '"-1000000000000000000000000000000000000000"'),
            ("SELECT 3.5e38::float4", '"350000000000000000000000000000000000000"'),
            # From text: the input verbatim.
            ("SELECT '1e39'::float4", '"1e39"'),
            ("SELECT '1e-50'::float4", '"1e-50"'),
            ("SELECT '1e400'::float8", '"1e400"'),
            # Underflow to zero is equally out of range — and `float('1e-400')`
            # being 0.0 is exactly why the "was the input zero?" test has to ask
            # the TEXT, not the float it parses to.
            ("SELECT '1e-400'::float8", '"1e-400"'),
        ],
    )
    def test_quoted_input_form(self, db, sql, want):
        exc = _err(db, sql)
        assert exc.sqlstate == "22003"
        assert exc.message.startswith(want)
        assert exc.message.endswith(
            "is out of range for type real"
            if "float4" in sql
            else "is out of range for type double precision"
        )

    @pytest.mark.parametrize("sql", ["SELECT 1e39::float8::float4", "SELECT (1e39::float8)::real"])
    def test_narrowing_a_double_uses_the_checkfloatval_form(self, db, sql):
        exc = _err(db, sql)
        assert exc.sqlstate == "22003"
        assert exc.message == "value out of range: overflow"

    def test_float4_out_of_range_was_an_internal_error(self, db):
        """`struct.pack('!f', 1e300)` raises `OverflowError`, which reached the
        wire as `XX000 internal error`."""
        assert _err(db, "SELECT (1e300)::float4").sqlstate == "22003"

    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT 1.5::float4", 1.5),
            # The smallest single-precision subnormal is in range; one decade
            # below it is not. The value here is the NARROWED double — over the
            # wire float4out renders it back as the shortest decimal that
            # round-trips, `1e-45`, which is what psycopg shows against both
            # servers. These tests call `run_sql` in-process and so see the
            # value before that rendering.
            ("SELECT 1e-45::float4", 1.401298464324817e-45),
            # The cast narrows, and the narrowed double is what is stored.
            ("SELECT 0.1::float4::float8", 0.10000000149011612),
            ("SELECT 3.4e38::float4 * 2", 6.7999999042887285e38),
        ],
    )
    def test_in_range_float4(self, db, sql, want):
        assert db(sql)[0] == [(want,)]

    def test_just_below_the_subnormal_floor(self, db):
        assert _err(db, "SELECT 1e-46::float4").sqlstate == "22003"
