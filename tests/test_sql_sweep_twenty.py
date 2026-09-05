"""A twentieth differential sweep — `sum()` over a group that contributed nothing.

The backlog had this filed as a 16-site refactor: a Mongo `$group` accumulator
is a single operator, none of them sums-or-nulls, so `sum` would need a
companion counter wired at every registration site. **That estimate was wrong,
and so was mine** — I counted the sites myself in an earlier batch and wrote
"16" into the backlog.

Reproducing it first showed why. The companion-counter mechanism already
existed (`_guard_sum_null`) and was already wired at six sites. `sum` over an
all-NULL group was already correct. What was broken was one line inside the
guard: it counted contributions with a bare `$ne: [value, null]`, and under
mongod's missing-vs-null rule that is **true for a MISSING field**. An
unmatched outer-join row carries no key at all for the non-driving side, so the
guard counted a contribution that never happened and kept the 0.

That is exactly why the bug looked so specific:
`GROUP BY` over a row whose value is genuinely NULL was right, and only the
LEFT JOIN with no matching row was wrong. The `COUNT(col)` accumulator three
functions away carries a comment describing this same trap and the same
`$ifNull` fix.

The other half was two `HAVING` paths that registered the accumulator without
calling the guard at all — the backlog explicitly dismissed those as "2
HAVING-dedup paths that need no companion". They need it: without the guard
`HAVING sum(x) IS NULL` can never be true.

Every expectation here was measured against PostgreSQL 14.13.
"""

from __future__ import annotations

import pytest

from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage

psycopg = pytest.importorskip("psycopg")


@pytest.fixture
def conn(tmp_path):
    st = Storage(str(tmp_path / "s20"))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    host, port = srv.address
    c = psycopg.connect(host=host, port=port, dbname="db", user="joe", autocommit=True)
    try:
        yield c
    finally:
        c.close()
        srv.stop()
        st.close()


@pytest.fixture
def joined(conn):
    """`j` row 3 has NO matching `k` row (so its k columns are ABSENT), while
    row 2 matches a k row whose value is NULL. Those two cases went down
    different paths, and only the first was wrong."""
    conn.execute("CREATE TABLE j (id int PRIMARY KEY, g text)")
    conn.execute("CREATE TABLE k (id int PRIMARY KEY, jid int, v int, w numeric)")
    conn.execute("INSERT INTO j VALUES (1,'x'),(2,'y'),(3,'z')")
    conn.execute("INSERT INTO k VALUES (10,1,5,5.5),(11,1,7,7.5),(12,2,NULL,NULL)")
    return conn


@pytest.fixture
def single(conn):
    conn.execute("CREATE TABLE ag (id int PRIMARY KEY, g text, n int)")
    conn.execute("INSERT INTO ag VALUES (1,'a',1),(2,'a',NULL),(3,'b',NULL)")
    return conn


LJ = "FROM j LEFT JOIN k ON k.jid = j.id"


# --- the unmatched outer-join row -------------------------------------------- #


def test_sum_over_an_unmatched_row_is_null(joined):
    assert joined.execute(f"SELECT j.id, sum(k.v) {LJ} GROUP BY j.id ORDER BY j.id").fetchall() == [
        (1, 12),
        (2, None),
        (3, None),
    ]


def test_sum_of_numeric_over_an_unmatched_row_is_null(joined):
    assert joined.execute(f"SELECT j.id, sum(k.w) {LJ} GROUP BY j.id ORDER BY j.id").fetchall() == [
        (1, 13),
        (2, None),
        (3, None),
    ]


def test_coalesce_sees_the_null(joined):
    """The shape that makes the old behaviour unmistakable: a caller supplying
    a default got 0 instead, because the sum was not NULL to begin with."""
    assert joined.execute(
        f"SELECT j.id, coalesce(sum(k.v), -1) {LJ} GROUP BY j.id ORDER BY j.id"
    ).fetchall() == [(1, 12), (2, -1), (3, -1)]


def test_grouping_by_another_column(joined):
    assert joined.execute(f"SELECT j.g, sum(k.v) {LJ} GROUP BY j.g ORDER BY j.g").fetchall() == [
        ("x", 12),
        ("y", None),
        ("z", None),
    ]


def test_ungrouped_sum_over_no_matching_rows(joined):
    assert joined.execute(f"SELECT sum(k.v) {LJ} WHERE j.id = 3").fetchall() == [(None,)]


def test_count_is_still_zero_not_null(joined):
    """`count` is the accumulator that SHOULD fold to zero. The guard must not
    have leaked into it."""
    assert joined.execute(
        f"SELECT j.id, count(k.v), count(*) {LJ} GROUP BY j.id ORDER BY j.id"
    ).fetchall() == [(1, 2, 2), (2, 0, 1), (3, 0, 1)]


def test_avg_min_max_were_already_right(joined):
    assert joined.execute(
        f"SELECT j.id, avg(k.v), min(k.v), max(k.v) {LJ} GROUP BY j.id ORDER BY j.id"
    ).fetchall() == [(1, 6, 5, 7), (2, None, None, None), (3, None, None, None)]


# --- HAVING ------------------------------------------------------------------ #


def test_having_sum_is_null_over_a_join(joined):
    assert joined.execute(
        f"SELECT j.id {LJ} GROUP BY j.id HAVING sum(k.v) IS NULL ORDER BY j.id"
    ).fetchall() == [(2,), (3,)]


def test_having_sum_is_null_single_table(single):
    assert single.execute(
        "SELECT g FROM ag GROUP BY g HAVING sum(n) IS NULL ORDER BY g"
    ).fetchall() == [("b",)]


def test_having_sum_is_not_null_still_works(joined):
    assert joined.execute(
        f"SELECT j.id {LJ} GROUP BY j.id HAVING sum(k.v) IS NOT NULL ORDER BY j.id"
    ).fetchall() == [(1,)]


def test_having_a_sum_comparison_is_unaffected(joined):
    assert joined.execute(
        f"SELECT j.id {LJ} GROUP BY j.id HAVING sum(k.v) > 5 ORDER BY j.id"
    ).fetchall() == [(1,)]


# --- the cases that were already correct, pinned against regression ---------- #


@pytest.mark.parametrize(
    "sql,expected",
    [
        ("SELECT g, sum(n) FROM ag GROUP BY g ORDER BY g", [("a", 1), ("b", None)]),
        ("SELECT sum(n) FROM ag WHERE id = 3", [(None,)]),
        ("SELECT sum(n) FROM ag WHERE false", [(None,)]),
        ("SELECT sum(n) FILTER (WHERE id > 99) FROM ag", [(None,)]),
        ("SELECT count(n) FILTER (WHERE id > 99) FROM ag", [(0,)]),
        ("SELECT sum(n) + 0 FROM ag WHERE false", [(None,)]),
    ],
)
def test_previously_correct_sum_shapes(single, sql, expected):
    assert single.execute(sql).fetchall() == expected
