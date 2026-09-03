"""An eleventh differential sweep — arrays, sequences, and VALUES subqueries.

Sequences and identity columns came back strong (23 of 26 shapes already
matching PostgreSQL 14.13). Arrays did not: 20 of 29, with the misses clustered
on MULTIDIMENSIONAL arrays, which Postgres does not treat as nested arrays at
all — `int[][]` is ONE array with two dimensions, and every whole-array
operation walks it flat.

**`array_to_string` leaked Python syntax.** Joining only the top level rendered
each inner list through `str()`, so `array_to_string(ARRAY[[1,2],[3,4]], ',')`
answered `[1, 2],[3, 4]` where Postgres says `1,2,3,4`.

**`unnest` of a 2-D array crashed** with a bare `invalid literal for int():
'{1,2}'` and no SQLSTATE — the inner lists went out as elements and the int4
output coercion died on them. It had THREE separate copies of the same
one-level logic (the `exp.Unnest` node, the Anonymous spelling, and the
select-list expansion); they share one helper now.

**Every scalar subquery over a VALUES source failed** with `42P01 relation ""
does not exist` — `IN (SELECT … FROM (VALUES …))`, `EXISTS`, `ARRAY(…)` and the
bare scalar form alike. A VALUES-derived source is not a relation, so the
inner-table lookup found nothing.

Every expectation here was measured against PostgreSQL 14.13.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture
def store(tmp_path):
    s = Storage(str(tmp_path / "s11"))
    try:
        yield s
    finally:
        s.close()


def _rows(store, sql, session):
    return [r for r in run_sql(store, "t", sql, session=session)][0].rows


def _tags(store, sql, session):
    res = [r for r in run_sql(store, "t", sql, session=session)][0]
    return [c.type_tag for c in res.columns]


@pytest.fixture
def sess(store):
    s = Session(database="t")
    _rows(store, "CREATE TABLE a11 (id int PRIMARY KEY, ia int[], m int[][])", s)
    _rows(store, "INSERT INTO a11 VALUES (1, ARRAY[1,2,3], ARRAY[[1,2],[3,4]])", s)
    return s


# --------------------------------------------------------------------------- #
# A multidimensional array is ONE array, walked flat
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    ("sql", "want"),
    [
        # Joining only the top level rendered the inner lists with str().
        ("SELECT array_to_string(ARRAY[[1,2],[3,4]], ',')", "1,2,3,4"),
        ("SELECT array_to_string(ARRAY[[1,2],[3,4]], '-')", "1-2-3-4"),
        ("SELECT array_to_string(ARRAY[1,2,3], ',')", "1,2,3"),
        ("SELECT array_to_string(ARRAY[1,NULL,3], ',', 'x')", "1,x,3"),
        ("SELECT array_to_string(ARRAY[]::int[], ',')", ""),
    ],
)
def test_array_to_string_flattens(store, sess, sql, want):
    assert _rows(store, sql, sess)[0][0] == want


def test_array_to_string_over_a_column(store, sess):
    assert _rows(store, "SELECT array_to_string(m, ',') FROM a11", sess) == [("1,2,3,4",)]


@pytest.mark.parametrize(
    "sql",
    [
        # Three routes into unnest, all of which had their own one-level copy.
        "SELECT unnest(ARRAY[[1,2],[3,4]])",
        "SELECT unnest(m) FROM a11",
    ],
)
def test_unnest_flattens_row_major(store, sess, sql):
    assert [r[0] for r in _rows(store, sql, sess)] == [1, 2, 3, 4]


def test_unnest_of_a_flat_array_is_unchanged(store, sess):
    assert [r[0] for r in _rows(store, "SELECT unnest(ARRAY[1,2,3])", sess)] == [1, 2, 3]


# --------------------------------------------------------------------------- #
# A VALUES source inside a scalar subquery
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    ("sql", "want"),
    [
        ("SELECT 1 WHERE 1 IN (SELECT n FROM (VALUES(1),(2)) t(n))", [(1,)]),
        ("SELECT 1 WHERE 9 IN (SELECT n FROM (VALUES(1),(2)) t(n))", []),
        ("SELECT (SELECT n FROM (VALUES(7)) t(n))", [(7,)]),
        ("SELECT EXISTS (SELECT 1 FROM (VALUES(1)) t(n))", [(True,)]),
        ("SELECT ARRAY(SELECT n FROM (VALUES(1),(2)) t(n))", [([1, 2],)]),
        # Default column names when the alias omits them.
        ("SELECT (SELECT column1 FROM (VALUES(5)) t)", [(5,)]),
        # An expression in the VALUES list is evaluated, not just read.
        ("SELECT (SELECT n FROM (VALUES(1 + 1)) t(n))", [(2,)]),
    ],
)
def test_values_source_in_a_subquery(store, sess, sql, want):
    assert _rows(store, sql, sess) == want


def test_values_subquery_keeps_its_element_type(store, sess):
    # Everything came back as text before, because the projection was untyped.
    assert _tags(store, "SELECT (SELECT n FROM (VALUES(7)) t(n))", sess) == ["int4"]
    assert _tags(store, "SELECT ARRAY(SELECT n FROM (VALUES(1),(2)) t(n))", sess) == ["int4[]"]


# --------------------------------------------------------------------------- #
# ARRAY(subquery)
# --------------------------------------------------------------------------- #


def test_array_subquery_over_a_table(store, sess):
    assert _rows(store, "SELECT ARRAY(SELECT id FROM a11 ORDER BY id)", sess) == [([1],)]


def test_array_subquery_with_no_rows_is_empty(store, sess):
    assert _rows(store, "SELECT ARRAY(SELECT id FROM a11 WHERE id > 99)", sess) == [([],)]


def test_plain_array_constructor_is_unchanged(store, sess):
    assert _rows(store, "SELECT ARRAY[1,2,3]", sess) == [([1, 2, 3],)]


# --------------------------------------------------------------------------- #
# pg_get_serial_sequence
# --------------------------------------------------------------------------- #


@pytest.fixture
def seq(store):
    s = Session(database="t")
    _rows(store, "CREATE TABLE sq11 (id serial PRIMARY KEY, big bigserial, n int)", s)
    return s


@pytest.mark.parametrize(
    ("col", "want"),
    [("id", "public.sq11_id_seq"), ("big", "public.sq11_big_seq"), ("n", None)],
)
def test_pg_get_serial_sequence(store, seq, col, want):
    # The column already recorded its sequence; this just never looked, so ORM
    # reflection saw every serial column as plain.
    assert _rows(store, f"SELECT pg_get_serial_sequence('sq11', '{col}')", seq) == [(want,)]


def test_pg_get_serial_sequence_accepts_a_qualified_table(store, seq):
    assert _rows(store, "SELECT pg_get_serial_sequence('public.sq11', 'id')", seq) == [
        ("public.sq11_id_seq",)
    ]
