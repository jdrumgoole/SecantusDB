"""`numeric(p, s)` never applied its declared scale.

Postgres ROUNDS a stored value to the declared scale: `0.12345` into a
`numeric(10,3)` column is stored as `0.123`, and a bare `1` as `1.000`. This
engine kept whatever scale the literal happened to carry, so the **stored value
itself** was wrong — not merely its rendering — and every `sum`, `min` / `max`,
`avg` and arithmetic result over the column inherited the error.

A value whose integer part still does not fit is `22003 numeric field
overflow`, never a truncation.

Every expectation here was measured against PostgreSQL 14.13, including the
error's DETAIL line.
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
        return [r.rows for r in run_sql(storage, "t", sql, session=session)][0]

    run("CREATE TABLE ns (id int, a numeric(10,3), b numeric(6,2), c numeric, d numeric(5,0))")
    run(
        "INSERT INTO ns VALUES (1, 1, 1, 1, 1), (2, 2.5, 2.5, 2.5, 2),"
        " (3, 0.12345, 0.12345, 0.12345, 3)"
    )
    try:
        yield run
    finally:
        storage.close()


def _texts(rows):
    return [str(r[0]) for r in rows]


class TestStoredValue:
    @pytest.mark.parametrize(
        ("col", "want"),
        [
            # Rounded to 3 places and padded to 3 places.
            ("a", ["0.123", "1.000", "2.500"]),
            ("b", ["0.12", "1.00", "2.50"]),
            # An UNCONSTRAINED numeric keeps its own scale, as PG does.
            ("c", ["0.12345", "1", "2.5"]),
        ],
    )
    def test_scale_is_applied(self, db, col, want):
        assert sorted(_texts(db(f"SELECT {col}::text FROM ns"))) == sorted(want)

    def test_scale_zero(self, db):
        assert sorted(_texts(db("SELECT d::text FROM ns"))) == ["1", "2", "3"]


class TestDerivedValues:
    """The point of storing the right value: everything computed from the
    column was wrong too."""

    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT sum(a)::text FROM ns", "3.623"),
            ("SELECT sum(b)::text FROM ns", "3.62"),
            ("SELECT min(a)::text FROM ns", "0.123"),
            ("SELECT max(a)::text FROM ns", "2.500"),
            ("SELECT avg(a)::text FROM ns", "1.20766666666666666667"),
        ],
    )
    def test_aggregates(self, db, sql, want):
        assert db(sql) == [(want,)]

    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT (a+b)::text FROM ns ORDER BY 1", ["0.243", "2.000", "5.000"]),
            ("SELECT (a*2)::text FROM ns ORDER BY 1", ["0.246", "2.000", "5.000"]),
        ],
    )
    def test_arithmetic(self, db, sql, want):
        assert _texts(db(sql)) == want


class TestRounding:
    @pytest.mark.parametrize(
        ("value", "want"),
        [
            ("1234.567", "1234.57"),
            # Half rounds AWAY FROM ZERO, both signs.
            ("-1234.567", "-1234.57"),
            ("0.005", "0.01"),
            ("-0.005", "-0.01"),
            ("0.004", "0.00"),
        ],
    )
    def test_half_away_from_zero(self, db, value, want):
        db(f"INSERT INTO ns (id, b) VALUES (9, {value})")
        assert db("SELECT b::text FROM ns WHERE id = 9") == [(want,)]

    def test_nan_has_no_scale(self, db):
        db("INSERT INTO ns (id, b) VALUES (9, 'NaN')")
        assert db("SELECT b::text FROM ns WHERE id = 9") == [("NaN",)]

    def test_null_is_untouched(self, db):
        db("INSERT INTO ns (id, b) VALUES (9, NULL)")
        assert db("SELECT b FROM ns WHERE id = 9") == [(None,)]


class TestOverflow:
    def _err(self, db, sql):
        with pytest.raises(SQLError) as exc:
            db(sql)
        return exc.value

    def test_insert_overflow(self, db):
        exc = self._err(db, "INSERT INTO ns (id, b) VALUES (9, 12345.6)")
        assert exc.sqlstate == "22003"
        assert exc.message == "numeric field overflow"
        assert exc.diag["D"] == (
            "A field with precision 6, scale 2 must round to an absolute value less than 10^4."
        )

    def test_update_overflow(self, db):
        assert self._err(db, "UPDATE ns SET b = 99999 WHERE id = 1").sqlstate == "22003"

    def test_overflow_is_decided_after_rounding(self, db):
        """`9999.999` rounds to `10000.00`, which no longer fits — PG checks the
        ROUNDED value, so this overflows even though the input's integer part
        fits."""
        assert self._err(db, "INSERT INTO ns (id, b) VALUES (9, 9999.999)").sqlstate == "22003"

    def test_just_inside_the_limit(self, db):
        db("INSERT INTO ns (id, b) VALUES (9, 9999.994)")
        assert db("SELECT b::text FROM ns WHERE id = 9") == [("9999.99",)]


class TestUpdatePath:
    def test_update_rounds(self, db):
        db("UPDATE ns SET b = 3.456 WHERE id = 1")
        assert db("SELECT b::text FROM ns WHERE id = 1") == [("3.46",)]

    def test_update_pads(self, db):
        db("UPDATE ns SET a = 7 WHERE id = 1")
        assert db("SELECT a::text FROM ns WHERE id = 1") == [("7.000",)]
