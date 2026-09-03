"""A ninth differential sweep — windows, and what RETURNING could not evaluate.

Both sides driven by the same psycopg client against PostgreSQL 14.13, so
client-side type mapping is identical and every difference is the server's.
Window functions were the richest surface yet probed here, and four of the
findings were SILENT:

**A named window was evaluated as ``OVER ()``.** sqlglot keeps a ``WINDOW w AS
(...)`` definition on the SELECT and leaves the *reference* as a bare alias with
no partition, no order and no frame — which is exactly what every consumer
downstream reads. So ``sum(v) OVER w`` with ``w AS (ORDER BY id)`` returned the
whole-partition total on every row instead of a running one, and a
``PARTITION BY`` in the definition was dropped just as quietly.

**``EXCLUDE`` was parsed and then ignored.** It sits on the frame spec as
``args["exclude"]`` and nothing read it, so ``EXCLUDE CURRENT ROW`` answered the
unexcluded frame — a running sum that still counted the current row.

**``NULLS FIRST`` in a window ORDER BY was ignored.** NULLs were placed by
DIRECTION alone, which happens to give Postgres' defaults (last for ASC, first
for DESC) — so the flag looked right until somebody wrote it explicitly, and
then every rank in the partition was wrong.

**``sum`` and ``avg`` were typed by rules the GROUP BY path had long since got
right.** A window ``sum(int4)`` declared int4 where Postgres promotes to int8,
and ``avg`` declared float8 and divided as a float where Postgres answers
numeric at ``select_div_scale``'s scale.

Plus: ``agg(...) FILTER (WHERE ...) OVER (...)`` was rejected with 42803 naming
an ordinary column, because the FILTER node sits between the aggregate and its
Window and the "is this a window aggregate?" guard looked only one level up;
``string_agg`` / ``array_agg`` were not window functions at all; ``ntile`` typed
as int8 where Postgres says int4; a subquery in RETURNING crashed with
``XX000``; and ``UPDATE ... SET (a, b) = (x, y)`` was refused outright.

Every expectation here was measured against PostgreSQL 14.13.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture
def store(tmp_path):
    s = Storage(str(tmp_path / "s9"))
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
    _rows(store, "CREATE TABLE w9 (id int PRIMARY KEY, g text, v int)", s)
    _rows(
        store,
        "INSERT INTO w9 VALUES (1,'a',10),(2,'a',20),(3,'a',20),(4,'b',5),(5,'b',NULL),(6,'b',30)",
        s,
    )
    return s


# --------------------------------------------------------------------------- #
# Named windows — evaluated as OVER () before
# --------------------------------------------------------------------------- #


def test_named_window_carries_its_order(store, sess):
    rows = _rows(store, "SELECT sum(v) OVER w FROM w9 WINDOW w AS (ORDER BY id) ORDER BY id", sess)
    assert [r[0] for r in rows] == [10, 30, 50, 55, 55, 85]


def test_named_window_carries_its_partition(store, sess):
    rows = _rows(
        store,
        "SELECT sum(v) OVER (w ORDER BY id) FROM w9 WINDOW w AS (PARTITION BY g) ORDER BY id",
        sess,
    )
    assert [r[0] for r in rows] == [10, 30, 50, 5, 5, 35]


def test_named_window_definitions_may_chain(store, sess):
    rows = _rows(
        store,
        "SELECT sum(v) OVER w2 FROM w9 "
        "WINDOW w1 AS (PARTITION BY g), w2 AS (w1 ORDER BY id) ORDER BY id",
        sess,
    )
    assert [r[0] for r in rows] == [10, 30, 50, 5, 5, 35]


def test_named_window_reference_may_add_a_frame(store, sess):
    rows = _rows(
        store,
        "SELECT sum(v) OVER (w ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM w9 "
        "WINDOW w AS (ORDER BY id) ORDER BY id",
        sess,
    )
    assert [r[0] for r in rows] == [10, 30, 40, 25, 5, 30]


def test_unknown_window_name(store, sess):
    with pytest.raises(errors.SQLError) as exc:
        _rows(store, "SELECT sum(v) OVER nope FROM w9", sess)
    assert exc.value.sqlstate == "42704"


def test_reference_may_not_override_the_definitions_order(store, sess):
    with pytest.raises(errors.SQLError) as exc:
        _rows(
            store,
            "SELECT sum(v) OVER (w ORDER BY v) FROM w9 WINDOW w AS (PARTITION BY g ORDER BY id)",
            sess,
        )
    assert exc.value.sqlstate == "42P20"


# --------------------------------------------------------------------------- #
# EXCLUDE — parsed, then ignored
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    ("clause", "want"),
    [
        ("EXCLUDE CURRENT ROW", [None, 10, 30, 50, 55, 55]),
        ("EXCLUDE NO OTHERS", [10, 30, 50, 55, 55, 85]),
        ("", [10, 30, 50, 55, 55, 85]),
    ],
)
def test_exclude_on_a_running_sum(store, sess, clause, want):
    rows = _rows(
        store,
        "SELECT sum(v) OVER (ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING "
        f"AND CURRENT ROW {clause}) FROM w9 ORDER BY id",
        sess,
    )
    assert [r[0] for r in rows] == want


def test_exclude_group_drops_the_whole_peer_group(store, sess):
    # Rows 2 and 3 are peers on v=20, so each loses both.
    rows = _rows(
        store,
        "SELECT sum(v) OVER (ORDER BY v ROWS BETWEEN UNBOUNDED PRECEDING "
        "AND UNBOUNDED FOLLOWING EXCLUDE GROUP) FROM w9 ORDER BY id",
        sess,
    )
    assert [r[0] for r in rows] == [75, 45, 45, 80, 85, 55]


def test_exclude_ties_keeps_the_current_row(store, sess):
    # A row with no peers loses nothing, so rows 1, 4, 5 and 6 keep the full sum.
    rows = _rows(
        store,
        "SELECT sum(v) OVER (ORDER BY v ROWS BETWEEN UNBOUNDED PRECEDING "
        "AND UNBOUNDED FOLLOWING EXCLUDE TIES) FROM w9 ORDER BY id",
        sess,
    )
    assert [r[0] for r in rows] == [85, 65, 65, 85, 85, 85]


def test_exclude_applies_to_value_windows_too(store, sess):
    rows = _rows(
        store,
        "SELECT first_value(id) OVER (ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING "
        "AND UNBOUNDED FOLLOWING EXCLUDE CURRENT ROW) FROM w9 ORDER BY id",
        sess,
    )
    assert [r[0] for r in rows] == [2, 1, 1, 1, 1, 1]


# --------------------------------------------------------------------------- #
# NULLS FIRST / LAST in a window ORDER BY
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    ("order", "want"),
    [
        ("v NULLS FIRST", [3, 4, 4, 2, 1, 6]),
        ("v NULLS LAST", [2, 3, 3, 1, 6, 5]),
        ("v", [2, 3, 3, 1, 6, 5]),  # ASC defaults to NULLS LAST
        ("v DESC", [5, 3, 3, 6, 1, 2]),  # DESC defaults to NULLS FIRST
        ("v DESC NULLS FIRST", [5, 3, 3, 6, 1, 2]),
        ("v DESC NULLS LAST", [4, 2, 2, 5, 6, 1]),
    ],
)
def test_window_order_nulls_placement(store, sess, order, want):
    rows = _rows(store, f"SELECT rank() OVER (ORDER BY {order}) FROM w9 ORDER BY id", sess)
    assert [r[0] for r in rows] == want


# --------------------------------------------------------------------------- #
# FILTER on a window aggregate
# --------------------------------------------------------------------------- #


def test_window_aggregate_filter(store, sess):
    rows = _rows(
        store,
        "SELECT count(*) FILTER (WHERE v > 10) OVER (ORDER BY id) FROM w9 ORDER BY id",
        sess,
    )
    assert [r[0] for r in rows] == [0, 1, 2, 2, 2, 3]


def test_window_aggregate_filter_does_not_demand_group_by(store, sess):
    # The FILTER node sits between the aggregate and its Window, so the guard
    # missed it and rejected `id` as needing a GROUP BY.
    rows = _rows(
        store,
        "SELECT id, sum(v) FILTER (WHERE g = 'a') OVER (ORDER BY id) FROM w9 ORDER BY id",
        sess,
    )
    assert rows == [(1, 10), (2, 30), (3, 50), (4, 50), (5, 50), (6, 50)]


# --------------------------------------------------------------------------- #
# string_agg / array_agg as window functions
# --------------------------------------------------------------------------- #


def test_string_agg_window(store, sess):
    rows = _rows(store, "SELECT string_agg(g, ',') OVER (ORDER BY id) FROM w9 ORDER BY id", sess)
    assert [r[0] for r in rows] == ["a", "a,a", "a,a,a", "a,a,a,b", "a,a,a,b,b", "a,a,a,b,b,b"]


def test_array_agg_window_keeps_nulls(store, sess):
    rows = _rows(
        store,
        "SELECT array_agg(v) OVER (ORDER BY id ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) "
        "FROM w9 ORDER BY id",
        sess,
    )
    assert [r[0] for r in rows] == [[10], [10, 20], [20, 20], [20, 5], [5, None], [None, 30]]


def test_array_agg_window_types_as_an_array(store, sess):
    assert _tags(store, "SELECT array_agg(v) OVER (ORDER BY id) FROM w9", sess) == ["int4[]"]


# --------------------------------------------------------------------------- #
# Window aggregate result types and exact avg
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    ("expr", "tag"),
    [
        ("sum(v)", "int8"),  # PG promotes sum(int4) to bigint
        ("avg(v)", "numeric"),  # ...and avg(int4) to numeric, not float8
        ("count(v)", "int8"),
        ("ntile(2)", "int4"),  # the one integer window function PG makes int4
        ("row_number()", "int8"),
        ("min(v)", "int4"),
    ],
)
def test_window_aggregate_types(store, sess, expr, tag):
    assert _tags(store, f"SELECT {expr} OVER (ORDER BY id) FROM w9", sess) == [tag]


def test_window_avg_is_exact_for_integer_input(store, sess):
    # Postgres finishes avg over an EXACT input in numeric at select_div_scale's
    # scale; dividing as a float lost the last digits.
    rows = _rows(store, "SELECT avg(v) OVER (PARTITION BY g) FROM w9 ORDER BY id", sess)
    assert str(rows[0][0]) == "16.6666666666666667"


# --------------------------------------------------------------------------- #
# RETURNING
# --------------------------------------------------------------------------- #


@pytest.fixture
def dml(store):
    s = Session(database="t")
    _rows(store, "CREATE TABLE d9 (id int PRIMARY KEY, n int DEFAULT 0, s text)", s)
    _rows(store, "INSERT INTO d9 VALUES (10, 1, 'a')", s)
    return s


def test_returning_may_contain_a_subquery(store, dml):
    # The RETURNING scalar context was built with catalog=None, so resolving the
    # subquery's table raised AttributeError -> XX000 internal error.
    rows = _rows(
        store, "INSERT INTO d9 VALUES (11, 2, 'b') RETURNING id, (SELECT min(id) FROM d9)", dml
    )
    assert rows == [(11, 10)]


def test_delete_returning_subquery(store, dml):
    rows = _rows(store, "DELETE FROM d9 WHERE id = 10 RETURNING id, (SELECT count(*) FROM d9)", dml)
    assert rows[0][0] == 10


# --------------------------------------------------------------------------- #
# UPDATE ... SET (a, b) = (x, y)
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    ("assignment", "want"),
    [
        ("(n, s) = (7, 'tup')", (10, 7, "tup")),
        ("(n) = ROW(9)", (10, 9, "a")),  # a one-element row parses as a Paren
        ("(n, s) = (n + 1, s || '!')", (10, 2, "a!")),
        ("(n, s) = ROW(3, 'r')", (10, 3, "r")),
    ],
)
def test_multi_column_set(store, dml, assignment, want):
    rows = _rows(store, f"UPDATE d9 SET {assignment} WHERE id = 10 RETURNING id, n, s", dml)
    assert rows == [want]


def test_multi_column_set_arity_mismatch(store, dml):
    with pytest.raises(errors.SQLError) as exc:
        _rows(store, "UPDATE d9 SET (n, s) = (1, 2, 3) WHERE id = 10", dml)
    assert exc.value.sqlstate == "42601"
