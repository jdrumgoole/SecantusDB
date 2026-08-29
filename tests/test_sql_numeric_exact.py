"""``numeric`` literals are exact decimals, not floats.

A decimal literal was read as a Python float, so Postgres' arbitrary-precision
exact ``numeric`` behaved like a double: ``0.1 + 0.2 = 0.3`` answered false,
``SELECT 0.000000`` came back as ``0``, and a value wider than a double
silently dropped digits. Every expectation here was checked against a real
PostgreSQL 14.13.

A ``numeric`` *column* already stored ``Decimal128`` (``typemap.coerce``), so
this makes literals and stored values share one representation rather than
introducing a new one.
"""

from __future__ import annotations

from decimal import Decimal

import pytest

from secantus.sql.engine import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture()
def q(tmp_path):
    storage = Storage(str(tmp_path))
    session = Session(database="t")

    def run(sql: str):
        return [r.rows for r in run_sql(storage, "t", sql, session=session)][0]

    try:
        yield run
    finally:
        storage.close()


def _one(rows):
    return rows[0][0]


class TestExactArithmetic:
    def test_the_canonical_float_surprise(self, q):
        assert Decimal(str(_one(q("SELECT 0.1 + 0.2")))) == Decimal("0.3")

    def test_equality_after_addition(self, q):
        assert _one(q("SELECT 0.1 + 0.2 = 0.3")) is True

    def test_value_wider_than_a_double_keeps_its_digits(self, q):
        assert str(_one(q("SELECT 12345678901234567890.12345 + 1"))) == (
            "12345678901234567891.12345"
        )

    @pytest.mark.parametrize(
        "sql,expected",
        [
            ("SELECT 2.5 * 4", "10.0"),
            ("SELECT 7.5 - 0.5", "7.0"),
            # Division carries PG's derived result scale (select_div_scale):
            # 10.0 / 4 renders 2.5000000000000000 on real 14.13, scale 16.
            ("SELECT 10.0 / 4", "2.5000000000000000"),
        ],
    )
    def test_arithmetic_keeps_decimal_semantics(self, q, sql, expected):
        assert str(_one(q(sql))) == expected


class TestScaleIsPreserved:
    @pytest.mark.parametrize(
        "sql,expected",
        [
            ("SELECT 0.000000", "0.000000"),
            ("SELECT 1.100000", "1.100000"),
            ("SELECT -0.000001", "-0.000001"),
            ("SELECT 0.100000", "0.100000"),
        ],
    )
    def test_trailing_zeros_survive(self, q, sql, expected):
        """pgjdbc's NumericTransfer2Test reads these with getBigDecimal and
        compares the scale, which is how the float conversion showed up."""
        assert str(_one(q(sql))) == expected


class TestTypesThatDoNotChange:
    def test_integers_stay_integers(self, q):
        assert _one(q("SELECT 3")) == 3
        assert _one(q("SELECT 10 / 4")) == 2  # integer division truncates

    @pytest.mark.parametrize(
        "sql,expected",
        [
            ("SELECT 1e3", "1000"),
            ("SELECT 1.5e2", "150"),
            ("SELECT 2e-3", "0.002"),
            ("SELECT 1e3 + 0.1", "1000.1"),
        ],
    )
    def test_exponent_notation_is_numeric_too(self, q, sql, expected):
        """``pg_typeof(1e3)`` is numeric, not float8 — checked against 14.13,
        which is not what the notation suggests."""
        assert str(_one(q(sql))) == expected


class TestComparisonsAgainstDecimals:
    def test_column_compared_to_a_decimal_expression(self, q):
        """The comparison operators swallowed the TypeError from comparing an
        int against a Decimal128 and answered false, so every such predicate
        silently matched nothing."""
        q("CREATE TABLE t (id int primary key, price int, cost int)")
        q("INSERT INTO t VALUES (1, 20, 10), (2, 12, 10), (3, 40, 30)")
        assert q("SELECT id FROM t WHERE price < cost * 1.5 ORDER BY id") == [(2,), (3,)]

    def test_stored_numeric_column_compares_to_a_literal(self, q):
        q("CREATE TABLE m (id int primary key, amount numeric(12,2))")
        q("INSERT INTO m VALUES (1, 10.50), (2, 20.25), (3, 5.00)")
        assert q("SELECT id FROM m WHERE amount > 9.99 ORDER BY id") == [(1,), (2,)]
        assert q("SELECT id FROM m WHERE amount = 20.25") == [(2,)]

    def test_sum_of_a_numeric_column_is_exact(self, q):
        q("CREATE TABLE s (id int primary key, amount numeric(12,2))")
        q("INSERT INTO s VALUES (1, 0.10), (2, 0.20)")
        assert Decimal(str(_one(q("SELECT sum(amount) FROM s")))) == Decimal("0.30")
