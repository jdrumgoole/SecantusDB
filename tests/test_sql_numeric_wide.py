"""`numeric` wider than Decimal128 is exact on the PYTHON PostgreSQL server.

Ported from the Rust server's own tests (`tests/test_rust_pgserver_slice.py`),
whose expectations PostgreSQL 16 produced from the same script. The Python
server stored every `numeric` as BSON Decimal128, so a value with more than 34
significant digits (or beyond Decimal128's exponent range) silently rounded --
a ceiling `docs/sql.md` documented as permanent. The Rust server closed it on
2026-09-09 with a two-form representation (`secantus.sql.numeric`); this is
the same contract, held by the Python server.
"""

from __future__ import annotations

import contextlib
from collections.abc import Iterator
from decimal import Decimal

import pytest

psycopg = pytest.importorskip("psycopg")

from secantus.sql.pgserver import SecantusPGServer  # noqa: E402
from secantus.storage import Storage  # noqa: E402


@contextlib.contextmanager
def _server(tmp_path) -> Iterator[psycopg.Connection]:
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        host, port = srv.address
        with psycopg.connect(
            host=host, port=port, user="postgres", dbname="postgres", autocommit=True
        ) as conn:
            yield conn
    finally:
        srv.stop()
        st.close()


_WIDE = Decimal("1.2345678901234567890123456789012345")  # 35 significant digits
_HUGE = Decimal("1e40")


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_numeric_wider_than_decimal128_round_trips(tmp_path, binary: bool) -> None:
    """A `numeric` with more than 34 significant digits, or beyond
    Decimal128's exponent range, round-trips EXACTLY with its display scale
    -- in the text and the binary formats -- where it used to be refused
    (22003). Every expectation is PostgreSQL 16's own rendering.
    """
    with _server(tmp_path) as conn:
        cur = conn.cursor(binary=binary)
        conn.execute("create table wn (id int primary key, n numeric)")
        rows = [
            (1, _WIDE),
            (2, _HUGE),
            (3, Decimal("-99999999999999999999999999999999999.5")),
            (4, Decimal("123456789012345678901234567890.123456789012345678901234567890")),
            (5, Decimal("1.000000000000000000000000000000000000000000")),
            (6, Decimal("1E-7000")),
            (7, Decimal("1.50")),
        ]
        cur.executemany("insert into wn values (%s, %s)", rows)
        cur.execute("select id, n from wn order by id")
        assert cur.fetchall() == rows
        cur.execute("select n::text from wn where id = 5")
        assert cur.fetchone() == ("1.000000000000000000000000000000000000000000",)
        cur.execute("select n * 2, n + 0.5, -n, abs(n), round(n, 3) from wn where id = 1")
        assert cur.fetchone() == (
            Decimal("2.4691357802469135780246913578024690"),
            Decimal("1.7345678901234567890123456789012345"),
            Decimal("-1.2345678901234567890123456789012345"),
            _WIDE,
            Decimal("1.235"),
        )
        # PostgreSQL's division scale rule, on a wide dividend.
        cur.execute("select n / 3 from wn where id = 1")
        assert cur.fetchone() == (Decimal("0.4115226300411522630041152263004115"),)
        # A computed numeric column is DESCRIBED as numeric (oid 1700), even
        # when the value is small: `n * 2` was typed int4 and the client's
        # int loader choked on `3.0`.
        assert cur.description[0].type_code == 1700
        # Beyond PostgreSQL's own limits is still refused, never rounded.
        with pytest.raises(psycopg.errors.NumericValueOutOfRange):
            cur.execute("select 1e131072::numeric")


def test_numeric_wider_than_decimal128_compares_and_sorts_exactly(tmp_path) -> None:
    """Mixed-width rows compare by VALUE in every WHERE operator and in ORDER
    BY (`1.50` ties `1.5`; the tie breaks on `id`), through a plain column and
    through a numeric PRIMARY KEY -- the `_id` index -- alike. NaN takes
    PostgreSQL's place, above infinity. Expectations were produced by
    PostgreSQL 16 from the same script.
    """
    vals = [
        "-1e40", "-99999999999999999999999999999999999.5", "-100", "-0.5", "0", "0.00",
        "0.5", "1.50", "1.5", "99999999999999999999999999999999999",
        "100000000000000000000000000000000000", "1.2345678901234567890123456789012345",
        "1.234567890123456789012345678901234", "1e40", "NaN", "Infinity", "-Infinity", "42",
    ]  # fmt: skip
    with _server(tmp_path) as conn:
        conn.execute("create table wn (id int primary key, n numeric)")
        cur = conn.cursor()
        for i, v in enumerate(vals):
            cur.execute("insert into wn values (%s, %s)", (i, Decimal(v)))
        cur.execute("insert into wn values (99, null)")

        def ids(sql: str, *args: object) -> list[int]:
            cur.execute(sql, args)
            return [r[0] for r in cur.fetchall()]

        assert ids("select id from wn order by n, id") == [
            16, 0, 1, 2, 3, 4, 5, 6, 12, 11, 7, 8, 17, 9, 10, 13, 15, 14, 99,
        ]  # fmt: skip
        assert ids("select id from wn order by n desc, id") == [
            99, 14, 15, 13, 10, 9, 17, 7, 8, 11, 12, 6, 4, 5, 3, 2, 1, 0, 16,
        ]  # fmt: skip
        assert ids("select id from wn where n = %s order by id", _WIDE) == [11]
        assert ids("select id from wn where n = %s order by id", _HUGE) == [13]
        assert ids("select id from wn where n = %s order by id", Decimal("1.50")) == [7, 8]
        assert ids("select id from wn where n > %s order by id", _WIDE) == [
            7, 8, 9, 10, 13, 14, 15, 17,
        ]  # fmt: skip
        assert ids("select id from wn where n < %s order by id", _WIDE) == [
            0, 1, 2, 3, 4, 5, 6, 12, 16,
        ]  # fmt: skip
        assert ids("select id from wn where n >= %s order by id", _HUGE) == [13, 14, 15]
        assert ids("select id from wn where n <> %s order by id", _HUGE) == [
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 14, 15, 16, 17,
        ]  # fmt: skip
        assert ids(
            "select id from wn where n in (%s, %s, %s) order by id",
            Decimal("1.5"), _HUGE, _WIDE,
        ) == [7, 8, 11, 13]  # fmt: skip
        assert ids(
            "select id from wn where n between %s and %s order by id", Decimal("0"), _WIDE
        ) == [4, 5, 6, 11, 12]
        # NaN: equal to itself, above infinity; `> NaN` is no row.
        nan = Decimal("NaN")
        assert ids("select id from wn where n = %s", nan) == [14]
        assert ids("select id from wn where n > %s", nan) == []
        assert ids("select id from wn where n > %s order by id", Decimal("Infinity")) == [14]
        assert ids("select id from wn where n < %s order by id", nan) == [
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 15, 16, 17,
        ]  # fmt: skip
        # `sum(numeric)` is exact and keeps the widest input scale.
        cur.execute("select sum(n), min(n), max(n) from wn where n < 1e100 and n > -1e100")
        assert cur.fetchone() == (
            Decimal("99999999999999999999999999999999946.9691357802469135780246913578024685"),
            Decimal("-1e40"),
            Decimal("1e40"),
        )
        assert cur.description[0].type_code == 1700

        # The same through a numeric PRIMARY KEY, where a wide `_id` is a
        # document the index keys by its text: equality, range, update and
        # delete resolve by value, and a duplicate that differs only in its
        # display scale is still a 23505.
        conn.execute("create table wpk (n numeric primary key, tag text)")
        for i, v in enumerate(["-1e40", "1.5", "1e40", str(_WIDE), "NaN"]):
            cur.execute("insert into wpk values (%s, %s)", (Decimal(v), f"t{i}"))
        for dup in ("1.50", "1e40", "10000000000000000000000000000000000000000.0", "NaN"):
            with pytest.raises(psycopg.errors.UniqueViolation):
                cur.execute("insert into wpk values (%s, 'dup')", (Decimal(dup),))
        cur.execute("select tag from wpk order by n")
        assert [r[0] for r in cur.fetchall()] == ["t0", "t3", "t1", "t2", "t4"]
        cur.execute("select tag from wpk where n = %s", (_HUGE,))
        assert cur.fetchall() == [("t2",)]
        cur.execute("select tag from wpk where n > %s order by tag", (Decimal("1.5"),))
        assert cur.fetchall() == [("t2",), ("t4",)]
        cur.execute("select tag from wpk where n < %s order by tag", (Decimal("1.5"),))
        assert cur.fetchall() == [("t0",), ("t3",)]
        cur.execute("update wpk set tag = 'upd' where n = %s", (_HUGE,))
        assert cur.rowcount == 1
        cur.execute("delete from wpk where n = %s", (_WIDE,))
        assert cur.rowcount == 1
        cur.execute("select count(*) from wpk")
        assert cur.fetchone() == (4,)


# --- Paths beyond the ported Rust spec, each a bug found while porting. -----


def _vals(cur, sql: str, *args: object) -> list:
    cur.execute(sql, args)
    return [r[0] for r in cur.fetchall()]


def test_pipeline_sorts_order_wide_values_by_value(tmp_path) -> None:
    """GROUP BY / DISTINCT / join / LIMIT sorts run in the pipeline, where BSON
    put every wide document above every Decimal128. They are now done after the
    pipeline (`planner._defer_numeric_sort`)."""
    want = [Decimal("-1e40"), _WIDE, Decimal("1.5"), Decimal("2"), _HUGE]
    with _server(tmp_path) as conn:
        conn.execute("create table t (id int primary key, g int, n numeric)")
        conn.execute("create table u (g int, label text)")
        cur = conn.cursor()
        cur.executemany(
            "insert into t values (%s, %s, %s)",
            [(1, 1, want[0]), (2, 1, want[2]), (3, 2, _WIDE), (4, 2, _HUGE), (5, 3, want[3])],
        )
        cur.execute("insert into u values (1, 'a'), (2, 'b'), (3, 'c')")
        assert _vals(cur, "select n from t group by n order by n") == want
        assert _vals(cur, "select distinct n from t order by n") == want
        assert _vals(cur, "select t.n from t join u on t.g = u.g order by t.n") == want
        assert _vals(cur, "select n from t group by n order by n desc limit 2") == [
            _HUGE,
            Decimal("2"),
        ]
        # ORDER BY an exact numeric aggregate, which is still a pushed marker
        # list inside the pipeline.
        assert _vals(cur, "select g from t group by g order by sum(n)") == [1, 3, 2]
        cur.execute("select id, rank() over (order by n) from t order by id")
        assert cur.fetchall() == [(1, 1), (2, 3), (3, 2), (4, 5), (5, 4)]


def test_updating_a_numeric_primary_key(tmp_path) -> None:
    """Re-keying hashed the old Decimal128 `_id`, which is unhashable: XX000."""
    with _server(tmp_path) as conn:
        conn.execute("create table pk (n numeric primary key, v text)")
        conn.execute("insert into pk values (1, 'a'), (2, 'b')")
        conn.execute("update pk set n = n + 10")
        cur = conn.execute("select n, v from pk order by n")
        assert cur.fetchall() == [(Decimal("11"), "a"), (Decimal("12"), "b")]


def test_exact_arithmetic_and_ranges(tmp_path) -> None:
    """`+ - *` ran in Python's 28-digit default context and `%` in floating
    point; a numrange bound rendered `1E+40`."""
    big = Decimal("1234567890123456789012345678901234567890")
    with _server(tmp_path) as conn:
        cur = conn.cursor()
        cur.execute("select (%s::numeric + 1)::text", (big,))
        assert cur.fetchone() == ("1234567890123456789012345678901234567891",)
        cur.execute("select (%s::numeric %% 7)::text", (big,))
        assert cur.fetchone() == (str(int(big) % 7),)
        cur.execute("select numrange(%s, %s)::text", (_WIDE, _HUGE))
        assert cur.fetchone() == (
            "[1.2345678901234567890123456789012345,10000000000000000000000000000000000000000)",
        )


def test_wide_range_bounds_in_the_binary_format(tmp_path) -> None:
    """The binary numeric encoder did `Decimal(str(value))` on the wide
    document: XX000 for any numrange / nummultirange with a wide bound read in
    the binary format (psycopg's random-data leak tests, 9 internal errors)."""
    from psycopg.types.multirange import Multirange
    from psycopg.types.range import Range

    with _server(tmp_path) as conn:
        conn.execute("create table r (id int primary key, nr numrange, nm nummultirange)")
        rng = Range(_WIDE, _HUGE, "[)")
        cur = conn.cursor(binary=True)
        cur.execute("insert into r values (1, %s, %s)", (rng, Multirange([rng])))
        cur.execute("select nr, nm from r")
        assert cur.fetchone() == (rng, Multirange([rng]))
