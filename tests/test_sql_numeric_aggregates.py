"""`avg`, `stddev` and `variance` over exact types answered the wrong type.

Postgres accumulates N, sum(X) and sum(X**2) as numerics and finishes in
numeric arithmetic, so an integer or numeric input gets an exact `numeric`
answer whose scale comes from `select_div_scale`. This engine used Mongo's
float accumulators — `$avg`, `$stdDevSamp` — and squared the stddev for the
variances, so every one of them was a `float8` where PostgreSQL says `numeric`,
and the values were off in the last digits with no scale at all:
`2.333333333333333` for PostgreSQL's `2.3333333333333333`.

`variance` and `var_pop` inside a computed projection (`variance(i)::text`)
were not supported AT ALL, because `_accumulator_for` has no post-aggregate
channel to finish them through.

A float input still answers `float8`, which is also what PostgreSQL does —
there only the tag was wrong, claiming `numeric` for a float result.

Every expectation here was measured against PostgreSQL 14.13.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture()
def db(tmp_path):
    storage = Storage(str(tmp_path))
    session = Session(database="t")

    def run(sql: str):
        res = [r for r in run_sql(storage, "t", sql, session=session)][0]
        return res.rows, [c.type_tag for c in res.columns]

    run("CREATE TABLE av (i int, b bigint, s smallint, n numeric, f float8, r real, g int)")
    run("INSERT INTO av VALUES (1,1,1,1,1,1,1),(2,2,2,2,2,2,1),(4,4,4,4.5,4,4,2)")
    try:
        yield run
    finally:
        storage.close()


def _one(db, sql):
    rows, _ = db(sql)
    return rows[0][0]


def _tag(db, sql):
    _rows, tags = db(sql)
    return tags[0]


class TestExactValues:
    """The whole point: an exact input gets Postgres' exact answer, digit for
    digit, at Postgres' derived scale."""

    @pytest.mark.parametrize("col", ["i", "b", "s"])
    @pytest.mark.parametrize(
        ("fn", "want"),
        [
            ("avg", "2.3333333333333333"),
            ("variance", "2.3333333333333333"),
            ("var_samp", "2.3333333333333333"),
            ("var_pop", "1.5555555555555556"),
            ("stddev", "1.5275252316519467"),
            ("stddev_samp", "1.5275252316519467"),
            ("stddev_pop", "1.2472191289246471"),
        ],
    )
    def test_integer_columns(self, db, col, fn, want):
        assert str(_one(db, f"SELECT {fn}({col}) FROM av")) == want

    @pytest.mark.parametrize(
        ("fn", "want"),
        [
            ("avg", "2.5000000000000000"),
            ("variance", "3.2500000000000000"),
            ("var_pop", "2.1666666666666667"),
            ("stddev", "1.8027756377319946"),
            ("stddev_pop", "1.4719601443879745"),
        ],
    )
    def test_numeric_column(self, db, fn, want):
        assert str(_one(db, f"SELECT {fn}(n) FROM av")) == want

    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            # The scale is DERIVED, not fixed: it depends on the operands'
            # weights, so 1/1 gets 20 places where 7/3 gets 16.
            ("SELECT avg(i) FROM av WHERE i = 1", "1.00000000000000000000"),
            ("SELECT avg(i) FROM av WHERE i IN (1,2)", "1.5000000000000000"),
            ("SELECT avg(n) FROM av WHERE n = 1", "1.00000000000000000000"),
        ],
    )
    def test_derived_scale(self, db, sql, want):
        assert str(_one(db, sql)) == want


class TestTypes:
    @pytest.mark.parametrize("fn", ["avg", "stddev", "stddev_pop", "variance", "var_pop"])
    @pytest.mark.parametrize("col", ["i", "b", "s", "n"])
    def test_exact_input_is_numeric(self, db, fn, col):
        assert _tag(db, f"SELECT {fn}({col}) FROM av") == "numeric"

    @pytest.mark.parametrize("fn", ["avg", "stddev", "variance", "var_pop"])
    @pytest.mark.parametrize("col", ["f", "r"])
    def test_float_input_is_float8(self, db, fn, col):
        """PG answers float8 for a float input. The variances claimed `numeric`
        for a value that was a float all along."""
        assert _tag(db, f"SELECT {fn}({col}) FROM av") == "float8"


class TestEdgeCases:
    @pytest.mark.parametrize("fn", ["stddev", "stddev_samp", "variance", "var_samp"])
    def test_sample_of_one_row_is_null(self, db, fn):
        assert _one(db, f"SELECT {fn}(i) FROM av WHERE i = 1") is None

    @pytest.mark.parametrize("fn", ["stddev_pop", "var_pop"])
    def test_population_of_one_row_is_plain_zero(self, db, fn):
        """PG returns `const_zero` outright when the numerator is not positive
        — a plain `0`, scale 0. Dividing instead would have answered
        `0.00000000000000000000`."""
        assert str(_one(db, f"SELECT {fn}(i) FROM av WHERE i = 1")) == "0"

    @pytest.mark.parametrize("fn", ["avg", "stddev", "variance"])
    def test_no_rows_is_null(self, db, fn):
        assert _one(db, f"SELECT {fn}(i) FROM av WHERE i > 100") is None

    def test_nulls_are_skipped(self, db):
        db("INSERT INTO av (i) VALUES (NULL)")
        assert str(_one(db, "SELECT avg(i) FROM av")) == "2.3333333333333333"


class TestGroupedAndComputed:
    @pytest.mark.parametrize(
        ("fn", "want"),
        [
            ("avg", ["1.5000000000000000", "4.0000000000000000"]),
            ("stddev", ["0.70710678118654752440", None]),
            ("stddev_pop", ["0.50000000000000000000", "0"]),
            ("variance", ["0.50000000000000000000", None]),
            ("var_pop", ["0.25000000000000000000", "0"]),
        ],
    )
    def test_grouped(self, db, fn, want):
        rows, _ = db(f"SELECT g, {fn}(i) FROM av GROUP BY g ORDER BY g")
        assert [None if r[1] is None else str(r[1]) for r in rows] == want

    @pytest.mark.parametrize(
        ("fn", "want"),
        [
            ("avg", "2.3333333333333333"),
            ("stddev", "1.5275252316519467"),
            # These two were not supported at all under a cast.
            ("variance", "2.3333333333333333"),
            ("var_pop", "1.5555555555555556"),
        ],
    )
    def test_inside_a_computed_projection(self, db, fn, want):
        assert _one(db, f"SELECT {fn}(i)::text FROM av") == want
