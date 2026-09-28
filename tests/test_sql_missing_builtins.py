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

import math

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
    """The expected value is the PLATFORM's libm, not the mathematical ideal.

    PostgreSQL's `dcbrt` is a bare libm `cbrt()` call, so whatever libm answers
    IS the oracle's answer. libm is not correctly rounded, and the error is not
    uniform: measured 2026-09-28 by calling `libm.so.6`'s `cbrt` directly
    through `ctypes` on **glibc 2.39**, `cbrt(27.0)` is `3.0000000000000004`
    while 8, 64, 125 and 1e6 come out exact; Windows (MSVC) answers exactly
    `3.0` for the same input, as does macOS.

    So `cbrt(27.0)` has no single right answer across platforms, and the
    hardcoded `3.0` these assertions used to carry was a Windows/macOS value.
    It failed on Linux for every Python that has `math.cbrt` (3.11+) and passed
    on 3.10, whose Newton-refined fallback rounds the ULP away -- which is why
    only the FULL matrix saw it: a push/PR run tests 3.10 on Linux and nothing
    else, so this sat red in the weekly cron instead.

    Being *more* exact than libm would move us AWAY from PostgreSQL --
    `_real_cbrt`'s own docstring says exactly that. So these assert to within
    one ULP, and `test_we_return_the_platform_libm_value` pins the stronger
    property that we hand libm's answer straight through.
    """

    @pytest.mark.parametrize(
        ("expr", "cube"),
        [("8", 2.0), ("27", 3.0), ("-8", -2.0), ("27.0", 3.0), ("1000000::numeric", 100.0)],
    )
    def test_cube_root(self, db, expr, cube):
        """One ULP, with no slack deliberately: glibc sits EXACTLY at the bound
        for 27.0 (measured -- the difference is `math.ulp(3.0)` to the bit), and
        1 ULP is libm's documented error for `cbrt`. A platform that exceeded it
        on a perfect cube would be worth a failure rather than a wider
        tolerance; the byte-exact property is pinned separately below."""
        (got,) = db(f"SELECT cbrt({expr})")[0][0]
        assert abs(got - cube) <= math.ulp(cube), f"{got!r} is more than 1 ULP from {cube!r}"

    def test_we_return_the_platform_libm_value(self, db):
        """Not merely close to the root -- byte-identical to what libm says.

        This is what stops someone "fixing" the ULP by hand-rolling a
        correctly-rounded cube root, which would disagree with PostgreSQL on
        ~8% of inputs. On 3.10 there is no `math.cbrt` and the fallback is
        deliberately exact on perfect cubes instead (see
        `test_python_310_fallback`), so the comparison only applies where the
        real thing is available.
        """
        libm_cbrt = getattr(math, "cbrt", None)
        if libm_cbrt is None:
            pytest.skip("no math.cbrt before 3.11; the fallback has its own test")
        for expr, arg in [("27.0", 27.0), ("27", 27.0), ("2", 2.0), ("10", 10.0)]:
            (got,) = db(f"SELECT cbrt({expr})")[0][0]
            assert got == libm_cbrt(arg), f"cbrt({expr}) diverged from libm"

    def test_a_one_ulp_libm_is_tolerated(self, db, monkeypatch):
        """A 1-ULP libm must survive the SQL layer UNROUNDED.

        Substitutes glibc's `cbrt(27.0)` (`3.0000000000000004`) for the
        platform's, so every machine exercises the value that only Linux
        produces. What this pins is the PLUMBING: nothing between `_cbrt` and
        the result row quietly rounds, re-derives or "corrects" libm's answer.

        It deliberately does NOT claim to make a hardcoded `3.0` elsewhere in
        this class fail on Windows or macOS -- it cannot, because it patches
        libm only for its own call. The guard against re-hardcoding is
        `test_we_return_the_platform_libm_value`, which derives the expectation
        from libm so there is no constant to hardcode. The full matrix remains
        the only place a platform-specific literal shows up, which is the point
        of this class's docstring.
        """
        from secantus.sql import scalar

        # `scalar.math` IS the math module, so capture the real function before
        # patching -- referring to `math.cbrt` inside the replacement would
        # recurse into the patch.
        original = getattr(math, "cbrt", None)
        if original is None:
            pytest.skip("no math.cbrt before 3.11; the fallback has its own test")
        monkeypatch.setattr(
            scalar.math,
            "cbrt",
            lambda x: 3.0000000000000004 if x == 27.0 else original(x),
            raising=False,
        )
        (got,) = db("SELECT cbrt(27.0)")[0][0]
        assert got == 3.0000000000000004, "the glibc value must survive the SQL layer unrounded"
        assert abs(got - 3.0) <= math.ulp(3.0)

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
        exist`. It is not missing — it was broken.

        Asserted to within one ULP for the platform-libm reason in this class's
        docstring: the point here is that a NUMERIC argument reaches the
        function at all, not the last bit of the result."""
        (got,) = db("SELECT cbrt(27.0)")[0][0]
        assert abs(got - 3.0) <= math.ulp(3.0)


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
