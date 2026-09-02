"""Functions PostgreSQL supports that SecantusDB refused.

The inverse of the internal-error hunt: instead of shapes where we crash, this
looked for shapes where **PostgreSQL succeeds and we error** — the direction
that finds missing functionality rather than leniency. 21 shapes across the
same function x value-type matrix; 7 remain, all deliberately.

One of the 21 was not a missing function at all but a real bug the
internal-error guard had just made *harder* to see: `cbrt(27.0)` raised
`TypeError` because `_cbrt` did `abs(v)` on a `Decimal128`, and the new guard
turned that into a plausible `42883 function cbrt(numeric) does not exist`.
Before the guard it was an obvious `XX000`. That is the cost of the guard, and
this diff-against-PostgreSQL sweep is what pays it back.
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
        res = [r for r in run_sql(storage, "t", sql, session=session)][0]
        return res.rows, [c.type_tag for c in res.columns]

    try:
        yield run
    finally:
        storage.close()


class TestIsfinite:
    @pytest.mark.parametrize(
        "value",
        [
            "date '2020-03-05'",
            "timestamp '2020-03-05 10:20:30'",
            "interval '3 days'",
            "time '10:20:30'",
        ],
    )
    def test_finite_values(self, db, value):
        rows, tags = db(f"SELECT isfinite({value})")
        assert rows == [(True,)]
        assert tags == ["bool"]

    def test_null_propagates(self, db):
        assert db("SELECT isfinite(NULL)")[0] == [(None,)]


class TestScale:
    @pytest.mark.parametrize(
        ("expr", "want"),
        [("1.50", 2), ("1", 0), ("1.5", 1), ("100", 0), ("0.001", 3)],
    )
    def test_digit_count_not_significance(self, db, expr, want):
        """`scale(1.50)` is 2 — the DECLARED digits, not the digits left after
        stripping trailing zeros."""
        rows, tags = db(f"SELECT scale({expr})")
        assert rows == [(want,)]
        assert tags == ["int4"]


class TestCbrt:
    @pytest.mark.parametrize(
        ("expr", "want"),
        [("8", 2.0), ("27", 3.0), ("-8", -2.0), ("27.0", 3.0), ("1000000::numeric", 100.0)],
    )
    def test_cube_root(self, db, expr, want):
        assert db(f"SELECT cbrt({expr})")[0] == [(want,)]

    def test_python_310_fallback(self, monkeypatch):
        """`math.cbrt` arrived in 3.11 and this package supports 3.10, so there
        are two code paths and each version's CI exercises only one of them.
        Pin the fallback everywhere: it must be exact on perfect cubes, which
        is the whole reason the power form was replaced."""
        from secantus.sql import scalar

        monkeypatch.delattr(scalar.math, "cbrt", raising=False)
        for value, want in [
            (8.0, 2.0),
            (27.0, 3.0),
            (-8.0, -2.0),
            (1000000.0, 100.0),
            (1e9, 1000.0),
            (0.0, 0.0),
        ]:
            assert scalar._real_cbrt(value) == want

    def test_a_numeric_argument_is_not_a_missing_overload(self, db):
        """`cbrt(27.0)` raised TypeError on the Decimal128, which the
        internal-error guard reported as `function cbrt(numeric) does not
        exist`. It is not missing — it was broken."""
        assert db("SELECT cbrt(27.0)")[0] == [(3.0,)]


class TestJustifyOnATime:
    """PostgreSQL coerces a `time` to an interval of that length."""

    @pytest.mark.parametrize("fn", ["justify_hours", "justify_days", "justify_interval"])
    def test_time_argument(self, db, fn):
        rows, _ = db(f"SELECT {fn}(time '10:20:30')")
        iv = rows[0][0]["interval"]
        assert (iv["months"], iv["days"]) == (0, 0)
        assert iv["micros"] == (10 * 3600 + 20 * 60 + 30) * 1_000_000

    def test_an_interval_argument_still_rolls_up(self, db):
        """The regression guard: a time is accepted WITHOUT changing what an
        interval does."""
        iv = db("SELECT justify_hours(interval '30 hours')")[0][0][0]["interval"]
        assert (iv["days"], iv["micros"]) == (1, 6 * 3600 * 1_000_000)

    def test_a_non_time_string_still_errors(self, db):
        with pytest.raises(SQLError):
            db("SELECT justify_hours('not a time')")
