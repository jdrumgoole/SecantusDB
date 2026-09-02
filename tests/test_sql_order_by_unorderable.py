"""`ORDER BY` on a `numeric` column was an internal error.

Sorting compares stored values directly, and `Decimal128` — which is EVERY
`numeric` and `money` value — implements no Python numeric protocol at all. So
`SELECT n FROM t ORDER BY n` answered `XX000 internal error`, hiding a bare
`TypeError: '<' not supported between instances of 'Decimal128' and
'Decimal128'`. An interval rides as a subdocument and had the same problem.

It reached every sort path that does not delegate to Mongo: a plain `ORDER BY`,
a window's `OVER (ORDER BY …)`, `array_agg(x ORDER BY x)`, `WITHIN GROUP
(ORDER BY …)`, and every window aggregate except `count` (the one that never
looks at the value). `DISTINCT`, `GROUP BY` and `UNION` were unaffected because
those sorts do go to Mongo — which is why the gap survived so long.

Decimal128 was also wrong where it did NOT raise: its equality compares the BID
encoding, so `1.0` and `1.00` were different values and `rank()` made two peers
into two ranks.

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
        return [r.rows for r in run_sql(storage, "t", sql, session=session)][0]

    run("CREATE TABLE o (n numeric, m money, iv interval, i int)")
    # 1.0 and 1.00 are numerically equal and differently scaled — the pair that
    # exposes both the comparison and the equality half of the bug.
    run(
        "INSERT INTO o VALUES (2.5, '2.50', interval '1 day', 1),"
        " (1.0, '1.50', interval '2 day', 2),"
        " (1.00, '1.50', interval '30 days', 3),"
        " (NULL, NULL, NULL, 4)"
    )
    try:
        yield run
    finally:
        storage.close()


def _texts(rows):
    return [str(r[0]) for r in rows]


class TestPlainOrderBy:
    @pytest.mark.parametrize(
        ("col", "want"),
        [
            ("n", ["1.0", "1.00", "2.5", "None"]),
            ("m", ["1.50", "1.50", "2.50", "None"]),
        ],
    )
    def test_ascending(self, db, col, want):
        assert _texts(db(f"SELECT {col} FROM o ORDER BY {col}")) == want

    def test_nulls_first_descending(self, db):
        rows = db("SELECT n FROM o ORDER BY n DESC NULLS FIRST")
        assert [str(r[0]) for r in rows][:2] == ["None", "2.5"]

    def test_interval_orders_by_duration(self, db):
        """Not by the subdocument's field order: `intervals.total_micros` is the
        value Postgres compares, so 1 day < 2 days < 30 days."""
        rows = db("SELECT i FROM o WHERE iv IS NOT NULL ORDER BY iv")
        assert [r[0] for r in rows] == [1, 2, 3]


class TestWindowOrderBy:
    def test_row_number(self, db):
        rows = db("SELECT i, row_number() OVER (ORDER BY n) FROM o ORDER BY i")
        assert dict(rows) == {1: 3, 2: 1, 3: 2, 4: 4}

    @pytest.mark.parametrize(
        ("fn", "want"),
        [
            # 1.0 and 1.00 are PEERS, so they share a rank. Decimal128 equality
            # compares the encoding, which made them two ranks.
            ("rank()", {1: 3, 2: 1, 3: 1, 4: 4}),
            ("dense_rank()", {1: 2, 2: 1, 3: 1, 4: 3}),
        ],
    )
    def test_peers_tie(self, db, fn, want):
        rows = db(f"SELECT i, {fn} OVER (ORDER BY n) FROM o ORDER BY i")
        assert dict(rows) == want

    def test_window_over_interval(self, db):
        rows = db("SELECT i, row_number() OVER (ORDER BY iv) FROM o ORDER BY i")
        assert dict(rows) == {1: 1, 2: 2, 3: 3, 4: 4}


class TestWindowAggregates:
    """Every one of these was an internal error except `count`, which never
    touches the value."""

    @pytest.mark.parametrize(
        ("agg", "want"),
        [
            # 4.50, not 4.5: Decimal addition keeps the widest operand scale,
            # and so does PG's.
            ("sum", ["2.5", "3.5", "4.50", "4.50"]),
            # Later wins a tie, as PG's `numeric_smaller` fold does — over
            # 2.5, 1.0, 1.00 the minimum is `1.00`, not `1.0`.
            ("min", ["2.5", "1.0", "1.00", "1.00"]),
            ("max", ["2.5", "2.5", "2.5", "2.5"]),
            ("count", ["1", "2", "3", "3"]),
        ],
    )
    def test_over_numeric(self, db, agg, want):
        assert _texts(db(f"SELECT {agg}(n) OVER (ORDER BY i) FROM o ORDER BY i")) == want

    def test_avg_over_numeric(self, db):
        # The SCALE still differs from PG (see `tasks/backlog.md`); the values do
        # not, and this used to raise rather than answer at all.
        assert _texts(db("SELECT avg(n) OVER (ORDER BY i) FROM o ORDER BY i")) == [
            "2.5",
            "1.75",
            "1.50",
            "1.50",
        ]

    def test_sum_over_interval(self, db):
        """A running interval total: 1 day, 3 days, 33 days."""
        rows = db("SELECT sum(iv) OVER (ORDER BY i) FROM o ORDER BY i")
        days = [r[0]["interval"]["days"] for r in rows]
        assert days == [1, 3, 33, 33]

    @pytest.mark.parametrize(("agg", "want"), [("min", 1), ("max", 30)])
    def test_min_max_over_interval(self, db, agg, want):
        rows = db(f"SELECT {agg}(iv) OVER (ORDER BY i) FROM o ORDER BY i")
        assert rows[-1][0]["interval"]["days"] == want


class TestOrderedSetAndArrayAgg:
    def test_array_agg_sorts(self, db):
        (values,) = db("SELECT array_agg(n ORDER BY n) FROM o")[0]
        assert [str(v) for v in values] == ["1.0", "1.00", "2.5", "None"]

    @pytest.mark.parametrize(
        ("expr", "want"),
        [
            ("percentile_disc(0.5) WITHIN GROUP (ORDER BY n)", "1.00"),
            ("mode() WITHIN GROUP (ORDER BY n)", "1.0"),
        ],
    )
    def test_within_group(self, db, expr, want):
        assert str(db(f"SELECT {expr} FROM o")[0][0]) == want

    def test_percentile_cont_interpolates(self, db):
        """Interpolation needs a value it can do arithmetic on, which is the
        other half of why `Decimal128` had to be unwrapped here."""
        assert db("SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY n) FROM o")[0][0] == 1


class TestUnaffectedPaths:
    """These sorts delegate to Mongo, which is why the bug stayed hidden."""

    @pytest.mark.parametrize(
        "sql",
        [
            "SELECT DISTINCT n FROM o ORDER BY n",
            "SELECT n FROM o GROUP BY n ORDER BY n",
            "SELECT n FROM o UNION SELECT n FROM o ORDER BY 1",
        ],
    )
    def test_still_ordered(self, db, sql):
        # 1.0 and 1.00 collapse to one row under DISTINCT/GROUP BY/UNION.
        assert _texts(db(sql))[-1] == "None"
